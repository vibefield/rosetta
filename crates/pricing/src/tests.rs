use super::*;

fn at(day: &str) -> Option<i64> {
    // Midday UTC on an ISO date, without a date library.
    let (year, rest) = day.split_once('-').unwrap();
    let (month, day) = rest.split_once('-').unwrap();
    let (year, month, day): (i64, i64, i64) = (
        year.parse().unwrap(),
        month.parse().unwrap(),
        day.parse().unwrap(),
    );
    // Days from civil (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * DAY_MS + DAY_MS / 2)
}

fn request<'a>(model: &'a str, day: &str) -> Request<'a> {
    Request {
        model,
        at_ms: at(day),
        ..Request::default()
    }
}

#[test]
fn exact_matching_never_uses_a_prefix() {
    assert_eq!(
        resolve("claude-opus-5").map(|m| m.id),
        Some("claude-opus-5")
    );
    // genai-prices' `starts_with: claude-opus-5` would price this as Opus 5.
    assert!(resolve("claude-opus-5-5").is_none());
    assert!(resolve("gpt-5.6-sol-pro").is_none());
}

#[test]
fn stamps_tags_and_vendor_prefixes_are_removed_before_the_exact_lookup() {
    let haiku = resolve("claude-haiku-4-5-20251001").map(|m| m.id);
    assert_eq!(haiku, Some("claude-haiku-4-5"));
    assert_eq!(
        resolve("claude-opus-4-6[1m]").map(|m| m.id),
        Some("claude-opus-4-6")
    );
    assert_eq!(
        resolve("Claude-Opus-4-6").map(|m| m.id),
        Some("claude-opus-4-6")
    );
    assert_eq!(
        resolve("gpt-5.6-sol-2026-07-10").map(|m| m.id),
        Some("gpt-5.6-sol")
    );
    assert_eq!(resolve("x-ai/grok-4.6").map(|m| m.id), Some("grok-4.6"));
    assert_eq!(resolve("k3").map(|m| m.id), Some("kimi-k3"));
    // Grok Build's spellings are cited aliases of the listed models.
    assert_eq!(resolve("grok-4.6-build").map(|m| m.id), Some("grok-4.6"));
    assert_eq!(resolve("grok-4.5-build").map(|m| m.id), Some("grok-4.5"));
    assert!(resolve("grok-4.7-build").is_none());
    assert!(resolve("<synthetic>").is_none());
}

#[test]
fn remembered_resolutions_equal_fresh_ones() {
    let ids = [
        "claude-opus-4-6",
        "Claude-Opus-4-6",
        "claude-haiku-4-5-20251001",
        "anthropic/claude-opus-4-6[1m]",
        "big-pickle",
        "gpt-5.6-sol",
        "grok-4.6-build",
        "k3",
        "claude-opus-5-5",
        "x-ai/grok-4.6",
    ];
    // More distinct ids than the memo holds, in varying order, twice over.
    for round in 0..3 {
        for index in 0..ids.len() {
            let id = ids[(index * (round + 1)) % ids.len()];
            assert_eq!(
                resolve(id).map(|m| m.id),
                resolve_uncached(id).map(|m| m.id),
                "{id}"
            );
        }
    }
}

#[test]
fn a_record_prices_at_the_period_in_force_on_its_own_day() {
    let mut before = request("gpt-5.6-sol", "2026-08-20");
    before.input = 100_000;
    before.output = 100_000;
    let mut after = before;
    after.at_ms = at("2026-08-21");
    // $5 + $30 per MTok before the 2026-08-21 cut, $4 + $20 from that day on.
    assert_eq!(list_cost(&before), Ok(3_500_000_000_000));
    assert_eq!(list_cost(&after), Ok(2_400_000_000_000));
    // A dated model without a timestamp is ambiguous, never guessed.
    after.at_ms = None;
    assert_eq!(list_cost(&after), Err(Unpriced::AmbiguousTime));
}

#[test]
fn long_context_tiers_reprice_the_whole_request_strictly_above_the_start() {
    // genai-prices encodes OpenAI's "< 272K" rate as a tier starting at 271,999.
    let mut at_limit = request("gpt-5.5", "2026-09-01");
    at_limit.input = 271_999;
    let mut above = at_limit;
    above.input = 272_000;
    assert_eq!(list_cost(&at_limit), Ok(271_999 * 5_000_000));
    // Every token, not only those past the threshold, moves to $10/MTok.
    assert_eq!(list_cost(&above), Ok(272_000 * 10_000_000));
    // Cached tokens count toward the prompt size that selects the tier.
    let mut cached = at_limit;
    cached.input = 1;
    cached.cache_read = 271_999;
    let quote = quote(&cached).unwrap();
    assert!(quote.long_context);
    assert_eq!(quote.pico_usd, 10_000_000 + 271_999 * 1_000_000);
}

#[test]
fn exclusive_buckets_each_use_their_own_rate() {
    let mut usage = request("claude-opus-4-8", "2026-08-01");
    usage.input = 1_000;
    usage.cache_read = 10_000;
    usage.cache_write = 2_000;
    usage.cache_write_1h = 3_000;
    usage.output = 400;
    usage.thinking = 600;
    usage.web_searches = 2;
    // $5 in, $0.50 read, $6.25 5-minute write, $10 1-hour write, $25 out
    // (thinking at the output rate), $10 per 1,000 searches.
    let expected = 1_000 * 5_000_000
        + 10_000 * 500_000
        + 2_000 * 6_250_000
        + 3_000 * 10_000_000
        + (400 + 600) * 25_000_000
        + 2 * 10_000_000_000;
    assert_eq!(list_cost(&usage), Ok(expected));
}

#[test]
fn a_missing_child_rate_falls_back_to_its_ancestor_like_genai_prices() {
    // gpt-5.4 has no cache-write rate: writes are charged as input.
    let mut usage = request("gpt-5.4", "2026-09-01");
    usage.cache_write = 1_000;
    assert_eq!(list_cost(&usage), Ok(1_000 * 2_500_000));
}

#[test]
fn service_tiers_price_only_through_a_cited_rule() {
    let mut usage = request("gpt-5.5", "2026-09-01");
    usage.input = 100_000;
    usage.service_tier = Some(ServiceTier::Priority);
    // 100K input at $5/MTok is $0.50; priority is 2.5x, flex 0.5x.
    assert_eq!(list_cost(&usage), Ok(1_250_000_000_000));
    usage.service_tier = Some(ServiceTier::Flex);
    assert_eq!(list_cost(&usage), Ok(250_000_000_000));
    usage.service_tier = Some(ServiceTier::Fast);
    assert_eq!(
        list_cost(&usage),
        Err(Unpriced::UnsupportedTier(ServiceTier::Fast))
    );
    let mut claude = request("claude-opus-4-8", "2026-09-01");
    claude.input = 1_000;
    claude.service_tier = Some(ServiceTier::Priority);
    assert_eq!(
        list_cost(&claude),
        Err(Unpriced::UnsupportedTier(ServiceTier::Priority))
    );
}

#[test]
fn tier_multipliers_scale_token_rates_never_per_call_fees() {
    let mut usage = request("gpt-5.5", "2026-09-01");
    usage.input = 100_000;
    usage.web_searches = 2;
    // $0.50 of input plus two searches at $10 per 1,000.
    assert_eq!(list_cost(&usage), Ok(500_000_000_000 + 20_000_000_000));
    // Priority is 2.5x the token rates; the searches cost what they did.
    usage.service_tier = Some(ServiceTier::Priority);
    assert_eq!(list_cost(&usage), Ok(1_250_000_000_000 + 20_000_000_000));
    usage.service_tier = Some(ServiceTier::Flex);
    assert_eq!(list_cost(&usage), Ok(250_000_000_000 + 20_000_000_000));
}

#[test]
fn provider_routes_are_informational_but_qualifiers_need_a_premium() {
    let mut usage = request("claude-opus-4-8", "2026-09-01");
    usage.input = 1_000;
    usage.route = Some("openrouter");
    assert_eq!(list_cost(&usage), Ok(1_000 * 5_000_000));
    usage.route = Some("anthropic/us");
    assert_eq!(list_cost(&usage), Err(Unpriced::UnsupportedRoute));
}

#[test]
fn uncounted_aggregates_price_only_below_every_tier_start() {
    let mut total = request("gpt-5.4", "2026-09-01");
    total.scope = UsageScope::Aggregate;
    total.input = 75_177;
    total.output = 5_956;
    assert!(list_cost(&total).is_ok());
    total.input = 900_000;
    assert_eq!(list_cost(&total), Err(Unpriced::AggregateAboveTier));
    // An untiered model has no tier to be ambiguous about.
    let mut flat = request("claude-opus-4-8", "2026-09-01");
    flat.scope = UsageScope::Aggregate;
    flat.input = 5_000_000;
    assert!(list_cost(&flat).is_ok());
}

#[test]
fn counted_aggregates_price_as_equal_requests_at_the_average_prompt() {
    // gpt-5.4 moves every token from $2.50 to $5.00 per MTok above 272K.
    let mut total = request("gpt-5.4", "2026-09-01");
    total.scope = UsageScope::Aggregate;
    total.input = 900_000;
    // Four requests average 225K prompt tokens: all at the base rate.
    total.requests = 4;
    assert_eq!(list_cost(&total), Ok(900_000 * 2_500_000));
    assert!(!quote(&total).unwrap().long_context);
    // Three average 300K: all above the tier.
    total.requests = 3;
    assert_eq!(list_cost(&total), Ok(900_000 * 5_000_000));
    assert!(quote(&total).unwrap().long_context);
    // The boundary is exact: 3 x 271,999 stays at the base rate.
    total.input = 3 * 271_999;
    assert_eq!(list_cost(&total), Ok(3 * 271_999 * 2_500_000));
    total.input += 1;
    assert_eq!(list_cost(&total), Ok((3 * 271_999 + 1) * 5_000_000));
    // A negative count is malformed, never a guess.
    total.requests = -1;
    assert_eq!(list_cost(&total), Err(Unpriced::NegativeCount));
}

#[test]
fn unpriced_is_never_zero() {
    let mut unknown = request("big-pickle", "2026-09-01");
    unknown.input = 10;
    assert_eq!(list_cost(&unknown), Err(Unpriced::UnknownModel));
    let mut negative = request("claude-opus-4-8", "2026-09-01");
    negative.input = -1;
    assert_eq!(list_cost(&negative), Err(Unpriced::NegativeCount));
}

#[test]
fn time_of_day_windows_wrap_midnight() {
    let window = Constraint::TimeOfDay {
        start_s: 22 * 3600,
        end_s: 2 * 3600,
    };
    assert!(window.active(23 * 3_600_000));
    assert!(window.active(DAY_MS + 3_600_000));
    assert!(!window.active(12 * 3_600_000));
    assert!(Constraint::FromDay(10).active(10 * DAY_MS));
    assert!(!Constraint::FromDay(10).active(10 * DAY_MS - 1));
}

#[test]
fn wire_conversion_rounds_once_half_away_from_zero() {
    assert_eq!(pico_to_micro(1_499_999), 1);
    assert_eq!(pico_to_micro(1_500_000), 2);
    assert_eq!(pico_to_micro(-1_500_000), -2);
    assert_eq!(pico_to_micro(0), 0);
}

#[test]
fn the_catalog_is_well_formed() {
    let aliases = generated::ALIASES;
    assert!(
        aliases.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "aliases sorted and unique"
    );
    for model in models() {
        assert!(!model.periods.is_empty(), "{} has a price", model.id);
        for period in model.periods {
            for rate in [
                period.prices.input,
                period.prices.output,
                period.prices.cache_read,
                period.prices.cache_write,
                period.prices.cache_write_1h,
                period.prices.output_reasoning,
            ]
            .into_iter()
            .flatten()
            {
                assert!(rate.base >= 0);
                assert!(
                    rate.tiers
                        .windows(2)
                        .all(|pair| pair[0].start < pair[1].start)
                );
            }
        }
    }
    assert!(CATALOG_VERSION.starts_with("genai-prices@"));
    assert_eq!(
        pricing_version(),
        format!("{CATALOG_VERSION}+calc{CALCULATOR_REVISION}")
    );
    assert!(tier_rules().iter().all(|rule| !rule.source.is_empty()));
}
