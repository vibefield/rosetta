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
    // genai-prices' old `starts_with: claude-opus-5` would have priced this
    // as Opus 5 (2.06x too high); it is its own catalog model.
    assert_eq!(
        resolve("claude-opus-5-5").map(|m| m.id),
        Some("claude-opus-5-5")
    );
    assert!(resolve("gpt-5.6-sol-pro").is_none());
    assert!(resolve("claude-opus-5-6").is_none());
}

#[test]
fn reseller_spellings_resolve_only_through_cited_aliases() {
    // Copilot, OpenRouter and Hermes spell Claude versions with a dot.
    assert_eq!(
        resolve("claude-fable-5.1").map(|m| m.id),
        Some("claude-fable-5-1")
    );
    assert_eq!(
        resolve("anthropic/claude-opus-5.5").map(|m| m.id),
        Some("claude-opus-5-5")
    );
    // Kimi Code plan ids.
    assert_eq!(resolve("k3-256k").map(|m| m.id), Some("kimi-k3"));
    assert_eq!(
        resolve("kimi-code/kimi-for-coding-highspeed").map(|m| m.id),
        Some("kimi-k2.7-code-highspeed")
    );
    // Grok Build's spellings of the listed models, including 4.7's Fast serving.
    assert_eq!(resolve("grok-4.7-build").map(|m| m.id), Some("grok-4.7"));
    assert_eq!(
        resolve("grok-4.7-build-fast").map(|m| m.id),
        Some("grok-4.7-fast")
    );
    assert!(resolve("grok-4.8-build").is_none());
}

#[test]
fn bedrock_and_cloudflare_ids_name_the_creators_models() {
    for (bedrock, expected) in [
        ("us.anthropic.claude-opus-4-6-v1", "claude-opus-4-6"),
        (
            "global.anthropic.claude-opus-4-5-20251101-v1:0",
            "claude-opus-4-5",
        ),
        (
            "anthropic.claude-haiku-4-5-20251001-v1:0",
            "claude-haiku-4-5",
        ),
        ("eu.anthropic.claude-sonnet-4-6-v1:0", "claude-sonnet-4-6"),
        ("openai.gpt-6-sol-v1", "gpt-6-sol"),
    ] {
        assert_eq!(resolve(bedrock).map(|m| m.id), Some(expected), "{bedrock}");
    }
    // Dots inside a plain id are not a vendor segment.
    assert_eq!(
        resolve("gemini-2.5-flash").map(|m| m.id),
        Some("gemini-2.5-flash")
    );
    assert!(resolve("us.unknownvendor.claude-opus-4-6-v1").is_none());
    // A leading `@` (Cloudflare Workers AI) is a prefix, not a version suffix.
    assert_eq!(
        resolve("@cf/moonshotai/kimi-k3").map(|m| m.id),
        Some("kimi-k3")
    );
    assert_eq!(
        resolve("claude-opus-4-6@20260301").map(|m| m.id),
        Some("claude-opus-4-6")
    );
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
    // OpenAI renamed Priority to Fast on 2026-07-30 and accepts both.
    usage.service_tier = Some(ServiceTier::Fast);
    assert_eq!(list_cost(&usage), Ok(1_250_000_000_000));
    // A model the Fast table does not list stays unpriced at that tier.
    let mut old = request("o3", "2026-09-01");
    old.input = 1_000;
    old.service_tier = Some(ServiceTier::Fast);
    assert_eq!(
        list_cost(&old),
        Err(Unpriced::UnsupportedTier(ServiceTier::Fast))
    );
    let mut claude = request("claude-opus-4-8", "2026-09-01");
    claude.input = 1_000;
    claude.service_tier = Some(ServiceTier::Priority);
    assert_eq!(
        list_cost(&claude),
        Err(Unpriced::UnsupportedTier(ServiceTier::Priority))
    );
    // Anthropic fast mode doubles every token category.
    claude.service_tier = Some(ServiceTier::Fast);
    claude.cache_read = 10_000;
    assert_eq!(
        list_cost(&claude),
        Ok(2 * (1_000 * 5_000_000 + 10_000 * 500_000))
    );
    // Google's tiers are provider-wide multipliers (100K prompt: base tier).
    let mut gemini = request("gemini-3.1-pro-preview", "2026-09-01");
    gemini.input = 100_000;
    gemini.service_tier = Some(ServiceTier::Priority);
    assert_eq!(list_cost(&gemini), Ok(100_000 * 2_000_000 * 9 / 5));
    gemini.service_tier = Some(ServiceTier::Batch);
    assert_eq!(list_cost(&gemini), Ok(100_000 * 2_000_000 / 2));
}

#[test]
fn dated_tier_rules_follow_the_record_day_and_need_a_timestamp() {
    // Opus 4.6 fast runs at standard rates from 2026-05-29; the earlier
    // research-preview premium is uncited, so those records stay unpriced.
    let mut usage = request("claude-opus-4-6", "2026-06-01");
    usage.input = 1_000;
    usage.service_tier = Some(ServiceTier::Fast);
    assert_eq!(list_cost(&usage), Ok(1_000 * 5_000_000));
    usage.at_ms = at("2026-05-28");
    assert_eq!(
        list_cost(&usage),
        Err(Unpriced::UnsupportedTier(ServiceTier::Fast))
    );
    // Opus 5.5 fast is dated too: an undated record cannot pick a rule.
    let mut dated = request("claude-opus-5-5", "2026-09-23");
    dated.input = 1_000;
    dated.service_tier = Some(ServiceTier::Fast);
    assert_eq!(list_cost(&dated), Ok(2 * 1_000 * 4_000_000));
    dated.at_ms = None;
    assert_eq!(list_cost(&dated), Err(Unpriced::AmbiguousTime));
    // An undated rule needs no timestamp (Opus 5 fast is undated, single period).
    let mut opus5 = request("claude-opus-5", "2026-09-01");
    opus5.input = 1_000;
    opus5.service_tier = Some(ServiceTier::Fast);
    opus5.at_ms = None;
    assert_eq!(list_cost(&opus5), Ok(2 * 1_000 * 5_000_000));
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
    // US-only inference is 1.1x on every token category for Claude 4.6+.
    usage.route = Some("anthropic/us");
    usage.cache_read = 10_000;
    usage.web_searches = 1;
    let tokens = 1_000 * 5_000_000 + 10_000 * 500_000;
    assert_eq!(
        list_cost(&usage),
        Ok((tokens * 11 + 5) / 10 + 10_000_000_000)
    );
    // Earlier models reject the parameter, so no premium is cited for them.
    let mut haiku = request("claude-haiku-4-5", "2026-09-01");
    haiku.input = 1_000;
    haiku.route = Some("anthropic/us");
    assert_eq!(list_cost(&haiku), Err(Unpriced::UnsupportedRoute));
    usage.route = Some("anthropic/eu");
    assert_eq!(list_cost(&usage), Err(Unpriced::UnsupportedRoute));
}

#[test]
fn patched_upstream_rates_and_new_models_price_from_the_overlay() {
    // kimi-k3's 1-hour writes are $6/MTok, twice the 5-minute $3 (patch).
    let mut kimi = request("k3", "2026-09-01");
    kimi.cache_write = 1_000;
    kimi.cache_write_1h = 1_000;
    assert_eq!(list_cost(&kimi), Ok(1_000 * 3_000_000 + 1_000 * 6_000_000));
    // grok-4.7 doubles at 200K prompt tokens, like grok-4.6; its Fast
    // serving is a separate price table.
    let mut grok = request("grok-4.7-build", "2026-09-23");
    grok.input = 199_999;
    assert_eq!(list_cost(&grok), Ok(199_999 * 2_000_000));
    grok.input = 200_000;
    assert_eq!(list_cost(&grok), Ok(200_000 * 4_000_000));
    let mut fast = request("grok-4.7-build-fast", "2026-09-23");
    fast.input = 1_000;
    fast.output = 100;
    assert_eq!(list_cost(&fast), Ok(1_000 * 4_000_000 + 100 * 12_000_000));
}

#[test]
fn a_future_price_period_starts_on_its_day() {
    // Gemini 3.7 Flash doubles on 2027-01-01 (announced in advance).
    let mut usage = request("gemini-3.7-flash", "2026-12-31");
    usage.input = 1_000_000;
    assert_eq!(list_cost(&usage), Ok(1_000_000 * 750_000));
    usage.at_ms = at("2027-01-01");
    assert_eq!(list_cost(&usage), Ok(1_000_000 * 1_500_000));
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
