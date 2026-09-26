#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignupChannel { Email, Sms }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentEvent { AccountOpened, CardLinked, TransferInitiated { cents: u64 } }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryChannel { Email, Sms }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Send(DeliveryChannel),
    HoldForConsent,
    NoDeliverableChannel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditDecision {
    pub event: PaymentEvent,
    pub decision: Decision,
    pub reason: &'static str,
}

pub fn decide_notification(
    event: PaymentEvent,
    signup: SignupChannel,
    consent_granted: bool,
    email_suppressed: bool,
    sms_suppressed: bool,
) -> AuditDecision {
    let sensitive = matches!(event, PaymentEvent::TransferInitiated { cents } if cents >= 100_000);
    if sensitive && !consent_granted {
        return AuditDecision { event, decision: Decision::HoldForConsent, reason: "high_value_transfer_requires_consent" };
    }

    let preferred = match signup { SignupChannel::Email => DeliveryChannel::Email, SignupChannel::Sms => DeliveryChannel::Sms };
    let available = |channel| match channel { DeliveryChannel::Email => !email_suppressed, DeliveryChannel::Sms => !sms_suppressed };
    let fallback = match preferred { DeliveryChannel::Email => DeliveryChannel::Sms, DeliveryChannel::Sms => DeliveryChannel::Email };
    let (decision, reason) = if available(preferred) {
        (Decision::Send(preferred), "signup_channel_available")
    } else if available(fallback) {
        (Decision::Send(fallback), "signup_channel_suppressed_fallback_available")
    } else {
        (Decision::NoDeliverableChannel, "all_recipients_suppressed")
    };
    AuditDecision { event, decision, reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_value_payment_is_held_until_consent_then_uses_sms_fallback() {
        let event = PaymentEvent::TransferInitiated { cents: 250_000 };
        assert_eq!(
            decide_notification(event, SignupChannel::Email, false, true, false).decision,
            Decision::HoldForConsent
        );
        let released = decide_notification(event, SignupChannel::Email, true, true, false);
        assert_eq!(released.decision, Decision::Send(DeliveryChannel::Sms));
        assert_eq!(released.reason, "signup_channel_suppressed_fallback_available");
    }
}

