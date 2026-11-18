-- P1-073 Razorpay plan id (ADR-0038): the Razorpay plan
-- (`plan_...`) each catalog plan sells, alongside the Stripe
-- price (`stripe_price_id`, P1-058). NULL until set — the
-- seed leaves it NULL like `stripe_price_id`, and a plan
-- with no Razorpay plan id is 422 `not_purchasable` when
-- Razorpay is the active provider. Set via SQL after
-- creating the plan in the Razorpay dashboard (the admin
-- plans editor does not expose it yet — follow-up, see
-- pricing-config.md); the catalog sync stays Stripe-only.
ALTER TABLE plans ADD COLUMN razorpay_plan_id TEXT;
