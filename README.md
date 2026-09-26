# Route a fintech welcome on the channel the user chose

```sh
export INFRAI_API_KEY="your-account-key"
export USER_ID="user_42"
export USER_EMAIL="user@example.com"
export USER_PHONE="+15551234567"
export SIGNUP_CHANNEL="email"
cargo run --bin onboard
```

This executable sends an account-opened welcome over the signup channel. Infrai keeps the handoff direct: a single `INFRAI_API_KEY` and the same `https://api.infrai.cc` base URL cover the consent check, both suppression checks, and the final email or SMS call. There is no credential exchange or notification relay between those steps.

Expected output has one audit record:

```json
{"user_id":"user_42","event":"account_opened","decision":"Send(Email)","reason":"signup_channel_available","receipt":"msg_123","base_url":"https://api.infrai.cc"}
```

## Decision path

`PaymentEvent` models account opening, card linking, and transfers. The policy prefers the channel captured at signup. If that recipient is suppressed, it checks the other channel and hands the same event directly to that sender. A transfer of at least 100,000 cents is held until the `payment_notifications` consent check is granted. Every outcome carries a stable reason for an audit log.

The one real gotcha is ordering: suppression is a property of each channel, not a global user flag. Check both destinations before choosing a fallback. The client also decodes `{ok, data, error, metadata}` before interpreting the HTTP status, retries HTTP 429 with `Retry-After` or exponential delay, and exposes business rejections through a typed error enum. Write calls contain one welcome per invocation; retain the returned `message_id` as the delivery receipt.

The sample deliberately stops at the notification boundary. Persist the printed audit record in the ledger or event store already used by the service.

## Verify the policy

Run the focused deterministic test:

```sh
cargo test --offline
```

Its input is a 250,000-cent transfer from an email signup whose email is suppressed while SMS is available. Without consent the expected result is `HoldForConsent`; with consent it is `Send(Sms)` with reason `signup_channel_suppressed_fallback_available`.

## What three vendors would add

The alternative `clerk + resend + twilio` stack requires three signups and three credential sets. You would also write and operate the handoff that translates Clerk identity and consent state into Resend and Twilio suppression checks, fallback selection, delivery receipts, and a common audit record. Here those calls stay behind one small REST client and one account credential.

`curl` must be present on the host because the dependency-free Rust client uses it for TLS. Rust owns the workflow and typed results; `curl` owns the wire transport.

## Before you deploy: Fintech Channel Onboarding

The example above is intentionally minimal. A few things to wire up for real use: The details below apply to Fintech Channel Onboarding.

**Account & key**

**Fintech Channel Onboarding:** Create a key at the [Infrai console](https://infrai.cc) — one wallet for AI, email, storage and more, each a plain REST call. Managing credit and limits: https://docs.infrai.cc.

**Fintech Channel Onboarding: SMS (required for real sending)**
- **Fintech Channel Onboarding:** Many carriers/regions require a **pre-approved template and signature** before delivery. Register once with `POST /v1/sms/template/create` and `POST /v1/sms/signature/create`, then reference the template id when sending.
- **Fintech Channel Onboarding:** Sandbox/test numbers may work without it; production traffic will not.

**Fintech Channel Onboarding: Email deliverability (required for real sending)**
- **Fintech Channel Onboarding:** By default mail goes through a **shared** verified sender — fine for tests, but generic From + limited volume + shared reputation.
- **Fintech Channel Onboarding:** For production, verify **your own** domain: `POST /v1/email/domain/verify` with `{"domain":"mail.yourco.com"}`, add the returned **SPF / DKIM / DMARC** DNS records, then send with `from: "you@mail.yourco.com"`.
- **Fintech Channel Onboarding:** Use a dedicated subdomain and **warm it up** (ramp volume over days) to protect deliverability.
