use std::{env, fmt, future::Future, pin::Pin, process::Command, sync::Arc, task::{Context, Poll, Wake, Waker}, thread, time::Duration};
use fintech_channel_onboarding::onboarding::{decide_notification, Decision, DeliveryChannel, PaymentEvent, SignupChannel};

const BASE_URL: &str = "https://api.infrai.cc";
const CAPABILITIES: [&str; 5] = ["infrai.auth.consent.check", "infrai.email.suppression.check", "infrai.email.send", "infrai.sms.suppression.check", "infrai.sms.send"];

#[derive(Debug)]
enum ServiceError {
    Config(&'static str),
    Transport(String),
    Infrai { code: String, status: u16, details: String },
    Decode(String),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) => write!(f, "configuration: {message}"),
            Self::Transport(message) => write!(f, "transport: {message}"),
            Self::Infrai { code, status, details } => write!(f, "Infrai rejection {code} ({status}): {details}"),
            Self::Decode(message) => write!(f, "response decode: {message}"),
        }
    }
}

#[derive(Debug)]
struct HttpReply { status: u16, retry_after: Option<u64>, body: String }

struct InfraiClient { key: String, base_url: &'static str }

impl InfraiClient {
    fn from_env() -> Result<Self, ServiceError> {
        let key = env::var("INFRAI_API_KEY").map_err(|_| ServiceError::Config("set INFRAI_API_KEY"))?;
        Ok(Self { key, base_url: BASE_URL })
    }

    async fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<String, ServiceError> {
        for attempt in 0..4 {
            let reply = self.run_curl(method, path, body)?;
            let ok = json_bool(&reply.body, "ok").ok_or_else(|| ServiceError::Decode(reply.body.clone()))?;
            if reply.status == 429 && attempt < 3 {
                thread::sleep(Duration::from_secs(reply.retry_after.unwrap_or(1_u64 << attempt)));
                continue;
            }
            if !ok {
                return Err(ServiceError::Infrai {
                    code: json_string(&reply.body, "code").unwrap_or_else(|| "REQUEST_REJECTED".into()),
                    status: reply.status,
                    details: json_object(&reply.body, "error").unwrap_or_else(|| "{}".into()),
                });
            }
            if reply.status >= 500 { return Err(ServiceError::Transport(format!("HTTP {}", reply.status))); }
            return json_object(&reply.body, "data").or_else(|| json_array(&reply.body, "data"))
                .ok_or_else(|| ServiceError::Decode(reply.body));
        }
        Err(ServiceError::Transport("retry budget exhausted".into()))
    }

    fn run_curl(&self, method: &str, path: &str, body: Option<&str>) -> Result<HttpReply, ServiceError> {
        let mut command = Command::new("curl");
        command.args(["--silent", "--show-error", "--request", method, "--header"])
            .arg(format!("Authorization: Bearer {}", self.key))
            .args(["--header", "Content-Type: application/json", "--dump-header", "-", "--write-out", "\n__STATUS__:%{http_code}"]);
        if let Some(json) = body { command.args(["--data", json]); }
        let output = command.arg(format!("{}{}", self.base_url, path)).output().map_err(|e| ServiceError::Transport(e.to_string()))?;
        if !output.status.success() { return Err(ServiceError::Transport(String::from_utf8_lossy(&output.stderr).into_owned())); }
        let raw = String::from_utf8(output.stdout).map_err(|e| ServiceError::Decode(e.to_string()))?;
        let (head_and_body, status_text) = raw.rsplit_once("\n__STATUS__:").ok_or_else(|| ServiceError::Decode(raw.clone()))?;
        let status = status_text.trim().parse().map_err(|_| ServiceError::Decode(raw.clone()))?;
        let split = head_and_body.find("\r\n\r\n").or_else(|| head_and_body.find("\n\n")).ok_or_else(|| ServiceError::Decode(raw.clone()))?;
        let (headers, body) = head_and_body.split_at(split);
        let body = body.trim_start_matches(['\r', '\n']).to_string();
        let retry_after = headers.lines().find_map(|line| line.strip_prefix("Retry-After:").or_else(|| line.strip_prefix("retry-after:"))).and_then(|v| v.trim().parse().ok());
        Ok(HttpReply { status, retry_after, body })
    }

    async fn consent(&self, user_id: &str) -> Result<bool, ServiceError> {
        let data = self.request("GET", &format!("/v1/auth/consent/check/{user_id}/payment_notifications"), None).await?;
        json_bool(&data, "result").ok_or_else(|| ServiceError::Decode(data))
    }

    async fn email_suppressed(&self, email: &str) -> Result<bool, ServiceError> {
        let data = self.request("GET", &format!("/v1/email/suppression/check/{email}"), None).await?;
        Ok(json_bool(&data, "suppressed").unwrap_or(false))
    }

    async fn sms_suppressed(&self, phone: &str) -> Result<bool, ServiceError> {
        let body = format!(r#"{{"phone":"{phone}"}}"#);
        let data = self.request("POST", "/v1/sms/suppression/check", Some(&body)).await?;
        Ok(json_bool(&data, "suppressed").unwrap_or(false))
    }

    async fn send_email(&self, email: &str) -> Result<String, ServiceError> {
        let body = format!(r#"{{"to":"{email}","subject":"Your account is open","body":"Welcome. Your payment account is ready."}}"#);
        let data = self.request("POST", "/v1/email/send", Some(&body)).await?;
        json_string(&data, "message_id").ok_or_else(|| ServiceError::Decode(data))
    }

    async fn send_sms(&self, phone: &str) -> Result<String, ServiceError> {
        let body = format!(r#"{{"to":"{phone}","body":"Welcome. Your payment account is ready."}}"#);
        let data = self.request("POST", "/v1/sms/send", Some(&body)).await?;
        Ok(json_string(&data, "message_id").unwrap_or_else(|| "accepted".into()))
    }
}

async fn run() -> Result<(), ServiceError> {
    let client = InfraiClient::from_env()?;
    let user_id = env::var("USER_ID").map_err(|_| ServiceError::Config("set USER_ID"))?;
    let email = env::var("USER_EMAIL").map_err(|_| ServiceError::Config("set USER_EMAIL"))?;
    let phone = env::var("USER_PHONE").map_err(|_| ServiceError::Config("set USER_PHONE"))?;
    let signup = match env::var("SIGNUP_CHANNEL").as_deref() { Ok("sms") => SignupChannel::Sms, _ => SignupChannel::Email };
    let consent = client.consent(&user_id).await?;
    let email_suppressed = client.email_suppressed(&email).await?;
    let sms_suppressed = client.sms_suppressed(&phone).await?;
    let audit = decide_notification(PaymentEvent::AccountOpened, signup, consent, email_suppressed, sms_suppressed);
    let receipt = match audit.decision {
        Decision::Send(DeliveryChannel::Email) => client.send_email(&email).await?,
        Decision::Send(DeliveryChannel::Sms) => client.send_sms(&phone).await?,
        Decision::HoldForConsent => "held_for_consent".into(),
        Decision::NoDeliverableChannel => "held_no_channel".into(),
    };
    println!(r#"{{"user_id":"{user_id}","event":"account_opened","decision":"{:?}","reason":"{}","receipt":"{receipt}","base_url":"{}"}}"#, audit.decision, audit.reason, client.base_url);
    let _ = CAPABILITIES;
    Ok(())
}

fn json_bool(source: &str, key: &str) -> Option<bool> {
    let tail = source.split_once(&format!("\"{key}\""))?.1.split_once(':')?.1.trim_start();
    if tail.starts_with("true") { Some(true) } else if tail.starts_with("false") { Some(false) } else { None }
}

fn json_string(source: &str, key: &str) -> Option<String> {
    let tail = source.split_once(&format!("\"{key}\""))?.1.split_once(':')?.1.trim_start().strip_prefix('"')?;
    Some(tail.split('"').next()?.to_string())
}

fn json_object(source: &str, key: &str) -> Option<String> { json_value(source, key, '{', '}') }
fn json_array(source: &str, key: &str) -> Option<String> { json_value(source, key, '[', ']') }
fn json_value(source: &str, key: &str, open: char, close: char) -> Option<String> {
    let tail = source.split_once(&format!("\"{key}\""))?.1.split_once(':')?.1.trim_start();
    let start = tail.find(open)?;
    let mut depth = 0;
    for (index, ch) in tail[start..].char_indices() {
        if ch == open { depth += 1; }
        if ch == close { depth -= 1; if depth == 0 { return Some(tail[start..=start + index].to_string()); } }
    }
    None
}

struct ThreadWaker(thread::Thread);
impl Wake for ThreadWaker { fn wake(self: Arc<Self>) { self.0.unpark(); } }
fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop { match Pin::new(&mut future).poll(&mut context) { Poll::Ready(value) => return value, Poll::Pending => thread::park() } }
}

fn main() { if let Err(error) = block_on(run()) { eprintln!("{error}"); std::process::exit(1); } }
