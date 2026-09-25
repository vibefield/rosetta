"""Compile the vendored genai-prices data plus Rosetta's cited overlay into the
static Rust catalog `crates/pricing/src/generated.rs`.

Everything here is exact: prices are parsed as decimals and converted to
integer pico-USD per token (or per call), and a price that does not convert
exactly is an error rather than a rounding. The committed output is the
reviewable artifact; CI runs `generate --check` to prove it is current.
"""

from __future__ import annotations

import hashlib
import json
import re
import tomllib
from dataclasses import dataclass, field
from datetime import date
from decimal import Decimal
from fractions import Fraction
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
CATALOG = ROOT / "crates" / "pricing" / "catalog"
UPSTREAM = CATALOG / "genai-prices" / "data.json"
UPSTREAM_SOURCE = CATALOG / "genai-prices" / "SOURCE.toml"
OVERLAY = CATALOG / "overlay.toml"
GENERATED = ROOT / "crates" / "pricing" / "src" / "generated.rs"

# Bumped with any change to how the tables are derived. The catalog version
# also carries a digest of the rendered tables, so a derivation change that
# alters them re-prices projections even if this constant is forgotten.
GENERATOR_VERSION = 2

# First-party model creators whose list prices define "API-equivalent" cost,
# in precedence order. A reseller table (OpenRouter, Copilot, Bedrock, Azure,
# Vertex's partner models) never feeds the headline figure. `None` admits
# every model of the provider; a tuple admits ids with one of the prefixes.
FIRST_PARTY: tuple[tuple[str, tuple[str, ...] | None], ...] = (
    ("anthropic", None),
    ("openai", None),
    ("x-ai", None),
    ("google", ("gemini", "gemma")),
    ("moonshotai", None),
    ("deepseek", None),
    ("mistral", None),
    ("zai", None),
    ("minimax", None),
)

# Upstream entries that are not the creator's list price and would otherwise
# price a record wrongly. They become `unknown_model` (unpriced, never wrong).
EXCLUDED_UPSTREAM: frozenset[tuple[str, str]] = frozenset(
    {
        # OpenAI publishes no API price for its open-weight models; genai-prices'
        # rows are "Imported from OpenRouter pricing" (its own price_comments).
        ("openai", "gpt-oss-120b"),
        ("openai", "gpt-oss-20b"),
        ("openai", "gpt-oss-safeguard-20b"),
        # OpenRouter's rate with no cache-hit price; Moonshot's own K2 previews
        # are `kimi-k2-0711-preview` / `kimi-k2-0905-preview`, and the overlay
        # aliases the bare id to the latter.
        ("moonshotai", "kimi-k2"),
        # Upstream's `mistral-small-latest` is Mistral Small 3.2's price; the
        # name has served Small 4 (`mistral-small-2603`) since 2026-03-16, and
        # the overlay aliases it there.
        ("mistral", "mistral-small-latest"),
    }
)

MTOK = Decimal(1_000_000)  # $/MTok -> pico-USD/token
KCOUNT = Decimal(1_000_000_000)  # $/1000 calls -> pico-USD/call
TIERS = ("standard", "priority", "flex", "batch", "fast")
PRICE_KEYS = {
    "input_mtok": ("input", MTOK),
    "output_mtok": ("output", MTOK),
    "cache_read_mtok": ("cache_read", MTOK),
    "cache_write_mtok": ("cache_write", MTOK),
    "cache_write_1h_mtok": ("cache_write_1h", MTOK),
    "output_reasoning_mtok": ("output_reasoning", MTOK),
    "web_searches_kcount": ("web_search_call", KCOUNT),
    "requests_kcount": ("request", KCOUNT),
}
TOKEN_FIELDS = ("input", "output", "cache_read", "cache_write", "cache_write_1h", "output_reasoning")
ALIAS_PATTERN = re.compile(r"^[a-z0-9][a-z0-9._:/+-]*$")


class PricingError(Exception):
    pass


def is_plain_id(model_id: str) -> bool:
    """A literal model id Rosetta can match exactly. Upstream prefix literals
    (`claude-sonnet-4@`, fine-tune stems like `ft:gpt-3.5-turbo-`) are not."""
    lowered = model_id.lower()
    return bool(ALIAS_PATTERN.match(lowered)) and not lowered.endswith(("@", "-", ".", "/", ":"))


@dataclass(frozen=True)
class Rate:
    base: int
    tiers: tuple[tuple[int, int], ...] = ()


@dataclass
class Period:
    constraint: tuple[str, Any]
    prices: dict[str, Rate | int]


@dataclass
class Model:
    provider: str
    id: str
    name: str
    periods: list[Period]
    aliases: list[str] = field(default_factory=list)


def _exact(value: Any, scale: Decimal, where: str) -> int:
    if isinstance(value, float):
        raise PricingError(f"{where}: float price {value!r}; prices must parse as decimals")
    scaled = Decimal(str(value)) * scale
    if scaled != scaled.to_integral_value() or scaled < 0:
        raise PricingError(f"{where}: {value} does not convert to a non-negative integer pico-USD rate")
    return int(scaled)


def _rate(value: Any, scale: Decimal, where: str) -> Rate | int:
    if isinstance(value, dict):
        if set(value) - {"base", "tiers"}:
            raise PricingError(f"{where}: unsupported tiered price keys {sorted(value)}")
        tiers = []
        previous = -1
        for step in value.get("tiers", []):
            start = step["start"]
            if not isinstance(start, int) or start <= previous:
                raise PricingError(f"{where}: tier starts must be increasing integers")
            previous = start
            tiers.append((start, _exact(step["price"], scale, where)))
        return Rate(_exact(value["base"], scale, where), tuple(tiers))
    if scale == KCOUNT:
        return _exact(value, scale, where)
    return Rate(_exact(value, scale, where))


def _prices(raw: dict[str, Any], where: str) -> dict[str, Rate | int]:
    prices: dict[str, Rate | int] = {}
    for key, value in raw.items():
        if key not in PRICE_KEYS:
            continue  # audio, image, storage-search, … units Rosetta never records
        name, scale = PRICE_KEYS[key]
        rate = _rate(value, scale, f"{where}.{key}")
        if scale == KCOUNT and isinstance(rate, Rate):
            raise PricingError(f"{where}.{key}: per-call prices cannot be tiered")
        prices[name] = rate
    return prices


def _seconds_of_day(value: str, where: str) -> int:
    match = re.fullmatch(r"(\d{2}):(\d{2}):(\d{2})(Z|[+-]\d{2}:\d{2})?", value)
    if not match:
        raise PricingError(f"{where}: unsupported time {value!r}")
    hours, minutes, seconds, zone = match.groups()
    offset = 0
    if zone and zone != "Z":
        sign = 1 if zone[0] == "+" else -1
        offset = sign * (int(zone[1:3]) * 3600 + int(zone[4:6]) * 60)
    return (int(hours) * 3600 + int(minutes) * 60 + int(seconds) - offset) % 86_400


def _day(value: Any, where: str) -> int:
    """An ISO date (TOML date or string) as days since 1970-01-01, UTC."""
    if isinstance(value, date):
        day = value
    elif isinstance(value, str):
        day = date.fromisoformat(value)
    else:
        raise PricingError(f"{where}: expected an ISO date, got {value!r}")
    return (day - date(1970, 1, 1)).days


def _rule_window(entry: dict[str, Any], where: str) -> tuple[int | None, int | None]:
    """An overlay rule's `from` (inclusive) and `until` (exclusive) days."""
    start = _day(entry["from"], f"{where}.from") if "from" in entry else None
    end = _day(entry["until"], f"{where}.until") if "until" in entry else None
    if start is not None and end is not None and end <= start:
        raise PricingError(f"{where}: `until` must be after `from`")
    return start, end


def _constraint(raw: dict[str, Any] | None, where: str) -> tuple[str, Any]:
    if not raw:
        return ("always", None)
    if set(raw) == {"start_date"}:
        return ("from_day", _day(raw["start_date"], where))
    if set(raw) == {"start_time", "end_time"}:
        return (
            "time_of_day",
            (_seconds_of_day(raw["start_time"], where), _seconds_of_day(raw["end_time"], where)),
        )
    raise PricingError(f"{where}: unsupported constraint {sorted(raw)}")


def _periods(raw: Any, where: str) -> list[Period]:
    entries = raw if isinstance(raw, list) else [{"prices": raw}]
    periods = []
    for index, entry in enumerate(entries):
        spot = f"{where}[{index}]"
        periods.append(Period(_constraint(entry.get("constraint"), spot), _prices(entry["prices"], spot)))
    return periods


def _match_aliases(clause: dict[str, Any]) -> list[str]:
    """Exact aliases only. `equals` is exact; a `starts_with` literal is kept as
    an exact alias of itself, never as a prefix. Regex, contains, ends_with and
    compound clauses are not aliases: date-stamped ids resolve by stripping the
    stamp at lookup time."""
    if "equals" in clause:
        return [clause["equals"]]
    if "starts_with" in clause:
        return [clause["starts_with"]]
    if "or" in clause:
        return [alias for inner in clause["or"] for alias in _match_aliases(inner)]
    return []


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _load_upstream() -> tuple[list[dict[str, Any]], dict[str, Any]]:
    source = tomllib.loads(UPSTREAM_SOURCE.read_text())
    actual = _sha256(UPSTREAM)
    if actual != source["sha256"]:
        raise PricingError(
            f"{UPSTREAM.relative_to(ROOT)} sha256 {actual} does not match SOURCE.toml {source['sha256']}"
        )
    return json.loads(UPSTREAM.read_text(), parse_float=Decimal), source


def _fraction(text: str, where: str) -> Fraction:
    try:
        value = Fraction(text)
    except (ValueError, ZeroDivisionError) as error:
        raise PricingError(f"{where}: bad multiplier {text!r}") from error
    if value <= 0:
        raise PricingError(f"{where}: multiplier must be positive")
    return value


def _require_source(entry: dict[str, Any], where: str) -> str:
    source = entry.get("source")
    if not isinstance(source, str) or not source.strip():
        raise PricingError(f"{where}: every overlay entry cites a source")
    return " ".join(source.split())


@dataclass
class Catalog:
    models: list[Model]
    aliases: dict[str, int]
    tier_rules: list[dict[str, Any]]
    route_rules: list[dict[str, Any]]
    sources: list[dict[str, str]]
    version: str


def load() -> Catalog:
    upstream, source = _load_upstream()
    overlay_bytes = OVERLAY.read_bytes()
    overlay = tomllib.loads(overlay_bytes.decode())
    providers = {provider["id"]: provider for provider in upstream}

    models: list[Model] = []
    for provider_id, prefixes in FIRST_PARTY:
        provider = providers.get(provider_id)
        if provider is None:
            raise PricingError(f"upstream data has no provider {provider_id!r}")
        for raw in provider["models"]:
            if prefixes is not None and not raw["id"].startswith(prefixes):
                continue
            if not is_plain_id(raw["id"]) or (provider_id, raw["id"]) in EXCLUDED_UPSTREAM:
                continue
            where = f"{provider_id}/{raw['id']}"
            periods = _periods(raw["prices"], where)
            if not any(
                any(name in period.prices for name in TOKEN_FIELDS) for period in periods
            ):
                continue  # image/audio/embedding-only models
            aliases = [raw["id"], *_match_aliases(raw.get("match", {}))]
            models.append(
                Model(provider_id, raw["id"], raw.get("name") or raw["id"], periods, aliases)
            )

    upstream_keys = {(model.provider, model.id) for model in models}
    for index, entry in enumerate(overlay.get("model", [])):
        where = f"overlay model[{index}]"
        _require_source(entry, where)
        if (entry["provider"], entry["id"]) in upstream_keys:
            # A same-id model would be appended after upstream's and never
            # resolved; rate corrections go through [[patch]].
            raise PricingError(
                f"{where}: {entry['provider']}/{entry['id']} is an upstream model; use [[patch]]"
            )
        models.append(
            Model(
                entry["provider"],
                entry["id"],
                entry.get("name", entry["id"]),
                _periods(_decimal_tree(entry["prices"]), where),
                [entry["id"], *entry.get("aliases", [])],
            )
        )

    by_key = {(model.provider, model.id): index for index, model in enumerate(models)}
    for index, entry in enumerate(overlay.get("patch", [])):
        # A cited correction of an upstream model's rates. A table of units
        # replaces (or adds) those units in every price period; a list of
        # dated periods replaces the model's price history outright.
        where = f"overlay patch[{index}]"
        _require_source(entry, where)
        target = by_key.get((entry["provider"], entry["model"]))
        if target is None:
            raise PricingError(f"{where}: no catalog model {entry['provider']}/{entry['model']}")
        prices = _decimal_tree(entry["prices"])
        if isinstance(prices, list):
            models[target].periods = _periods(prices, where)
            continue
        rates = _prices(prices, where)
        if not rates:
            raise PricingError(f"{where}: a patch names at least one price")
        for period in models[target].periods:
            period.prices.update(rates)

    aliases: dict[str, int] = {}
    for index, model in enumerate(models):
        for alias in dict.fromkeys(alias.lower() for alias in model.aliases):
            # Upstream prefix literals such as `claude-sonnet-4@` are not model
            # ids; the resolver strips `@version` suffixes itself.
            if not is_plain_id(alias):
                continue
            # FIRST_PARTY order is the precedence between creators.
            aliases.setdefault(alias, index)

    overlay_aliases: set[str] = set()
    for index, entry in enumerate(overlay.get("alias", [])):
        where = f"overlay alias[{index}]"
        _require_source(entry, where)
        target = by_key.get((entry["provider"], entry["model"]))
        if target is None:
            raise PricingError(f"{where}: no catalog model {entry['provider']}/{entry['model']}")
        alias = entry["alias"].lower()
        if not ALIAS_PATTERN.match(alias):
            raise PricingError(f"{where}: alias {alias!r} is not a plain model id")
        if alias in aliases and aliases[alias] != target:
            existing = models[aliases[alias]]
            # Re-pointing an upstream spelling is allowed only when the entry
            # names what it displaces, so the correction is deliberate.
            displaced = f"{existing.provider}/{existing.id}"
            if alias in overlay_aliases or entry.get("replaces") != displaced:
                raise PricingError(f"{where}: {alias!r} already names {displaced}")
        aliases[alias] = target
        overlay_aliases.add(alias)

    tier_rules = []
    for index, entry in enumerate(overlay.get("tier", [])):
        where = f"overlay tier[{index}]"
        cited = _require_source(entry, where)
        # `tier = "fast"` or `tiers = ["priority", "fast"]` for spellings a
        # provider accepts interchangeably.
        tiers = entry["tiers"] if "tiers" in entry else [entry["tier"]]
        for tier in tiers:
            if tier not in TIERS or tier == "standard":
                raise PricingError(f"{where}: tier must be one of {TIERS[1:]}")
        if "model" in entry and (entry["provider"], entry["model"]) not in by_key:
            raise PricingError(f"{where}: no catalog model {entry['provider']}/{entry['model']}")
        multiplier = _fraction(entry["multiplier"], where)
        from_day, until_day = _rule_window(entry, where)
        for tier in tiers:
            tier_rules.append(
                {
                    "provider": entry["provider"],
                    "model": entry.get("model"),
                    "tier": tier,
                    "numerator": multiplier.numerator,
                    "denominator": multiplier.denominator,
                    "from_day": from_day,
                    "until_day": until_day,
                    "source": cited,
                }
            )

    route_rules = []
    for index, entry in enumerate(overlay.get("route", [])):
        where = f"overlay route[{index}]"
        cited = _require_source(entry, where)
        if "/" not in entry["route"]:
            raise PricingError(f"{where}: only qualified routes carry premiums")
        for model_id in entry.get("models", []):
            if (entry["provider"], model_id) not in by_key:
                raise PricingError(f"{where}: no catalog model {entry['provider']}/{model_id}")
        multiplier = _fraction(entry["multiplier"], where)
        from_day, until_day = _rule_window(entry, where)
        route_rules.append(
            {
                "route": entry["route"],
                "provider": entry["provider"],
                "models": list(entry.get("models", [])),
                "numerator": multiplier.numerator,
                "denominator": multiplier.denominator,
                "from_day": from_day,
                "until_day": until_day,
                "source": cited,
            }
        )

    overlay_sha = hashlib.sha256(overlay_bytes).hexdigest()
    sources = [
        {
            "name": "pydantic/genai-prices",
            "url": source["url"],
            "revision": source["revision"],
            "sha256": source["sha256"],
            "license": source["license"],
        },
        {
            "name": "rosetta overlay",
            "url": "crates/pricing/catalog/overlay.toml",
            "revision": f"gen{GENERATOR_VERSION}",
            "sha256": overlay_sha,
            "license": "rosetta",
        },
    ]
    # The tables' own digest is appended by `build`, so the version changes
    # whenever anything a stored cost depends on changes.
    version = (
        f"genai-prices@{source['revision'][:12]}+data@{source['sha256'][:12]}"
        f"+overlay@{overlay_sha[:12]}+gen{GENERATOR_VERSION}"
    )
    if len(models) > 65_535:
        raise PricingError("too many models for a u16 alias index")
    return Catalog(models, aliases, tier_rules, route_rules, sources, version)


def _decimal_tree(value: Any) -> Any:
    """Overlay prices are strings so TOML never rounds them through a float."""
    if isinstance(value, dict):
        return {key: _decimal_tree(inner) for key, inner in value.items()}
    if isinstance(value, list):
        return [_decimal_tree(inner) for inner in value]
    if isinstance(value, str) and re.fullmatch(r"\d+(\.\d+)?", value):
        return Decimal(value)
    if isinstance(value, float):
        raise PricingError(f"overlay price {value!r} must be written as a string")
    return value


def _int(value: int) -> str:
    return f"{value:_}"


def _str(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def _rust_rate(rate: Rate | None) -> str:
    if rate is None:
        return "None"
    tiers = ", ".join(f"TierStep {{ start: {_int(start)}, rate: {_int(value)} }}" for start, value in rate.tiers)
    return f"Some(Rate {{ base: {_int(rate.base)}, tiers: &[{tiers}] }})"


def _rust_constraint(constraint: tuple[str, Any]) -> str:
    kind, value = constraint
    if kind == "always":
        return "Constraint::Always"
    if kind == "from_day":
        return f"Constraint::FromDay({value})"
    start, end = value
    return f"Constraint::TimeOfDay {{ start_s: {start}, end_s: {end} }}"


def _rust_money(value: int | None) -> str:
    return "None" if value is None else f"Some({_int(value)})"


def _rust_day(value: int | None) -> str:
    return "None" if value is None else f"Some({value})"


def render(catalog: Catalog) -> str:
    """The generated module. The version line is rendered last: it carries a
    digest of every table below it."""
    tables = render_tables(catalog)
    digest = hashlib.sha256(tables.encode()).hexdigest()[:12]
    version = f"{catalog.version}+tables@{digest}"
    head = [
        "// @generated by `python3 -m tools.pricing generate` from",
        "// crates/pricing/catalog/ (vendored genai-prices data + the Rosetta overlay).",
        "// Do not edit by hand; edit the overlay or re-vendor upstream and regenerate.",
        "",
        "use rosetta_core::ServiceTier;",
        "",
        "use crate::catalog::{",
        "    Constraint, Model, Period, Prices, Rate, RouteRule, Source, TierRule, TierStep,",
        "};",
        "",
        f"pub(crate) const CATALOG_VERSION: &str = {_str(version)};",
        "",
    ]
    return "\n".join(head) + tables


def render_tables(catalog: Catalog) -> str:
    out = [
        "pub(crate) static SOURCES: &[Source] = &[",
    ]
    for source in catalog.sources:
        out.append(
            "    Source { "
            f"name: {_str(source['name'])}, url: {_str(source['url'])}, "
            f"revision: {_str(source['revision'])}, sha256: {_str(source['sha256'])}, "
            f"license: {_str(source['license'])} }},"
        )
    out += ["];", "", "pub(crate) static MODELS: &[Model] = &["]
    for model in catalog.models:
        out.append(
            f"    Model {{ provider: {_str(model.provider)}, id: {_str(model.id)}, "
            f"name: {_str(model.name)}, periods: &["
        )
        for period in model.periods:
            prices = period.prices
            fields = [f"{name}: {_rust_rate(prices.get(name))}" for name in TOKEN_FIELDS]
            fields.append(f"web_search_call: {_rust_money(prices.get('web_search_call'))}")
            fields.append(f"request: {_rust_money(prices.get('request'))}")
            out.append(f"        Period {{ constraint: {_rust_constraint(period.constraint)}, prices: Prices {{")
            out.extend(f"            {field}," for field in fields)
            out.append("        } },")
        out.append("    ] },")
    out += ["];", "", "/// Sorted by alias for binary search; the value indexes `MODELS`.",
            "pub(crate) static ALIASES: &[(&str, u16)] = &["]
    for alias in sorted(catalog.aliases):
        out.append(f"    ({_str(alias)}, {catalog.aliases[alias]}),")
    out += ["];", "", "pub(crate) static TIER_RULES: &[TierRule] = &["]
    for rule in catalog.tier_rules:
        model = "None" if rule["model"] is None else f"Some({_str(rule['model'])})"
        out.append(
            f"    TierRule {{ provider: {_str(rule['provider'])}, model: {model}, "
            f"tier: ServiceTier::{rule['tier'].capitalize()}, numerator: {rule['numerator']}, "
            f"denominator: {rule['denominator']}, from_day: {_rust_day(rule['from_day'])}, "
            f"until_day: {_rust_day(rule['until_day'])}, source: {_str(rule['source'])} }},"
        )
    out += ["];", "", "pub(crate) static ROUTE_RULES: &[RouteRule] = &["]
    for rule in catalog.route_rules:
        models = ", ".join(_str(model) for model in rule["models"])
        out.append(
            f"    RouteRule {{ route: {_str(rule['route'])}, provider: {_str(rule['provider'])}, "
            f"models: &[{models}], numerator: {rule['numerator']}, "
            f"denominator: {rule['denominator']}, from_day: {_rust_day(rule['from_day'])}, "
            f"until_day: {_rust_day(rule['until_day'])}, source: {_str(rule['source'])} }},"
        )
    out += ["];", ""]
    return "\n".join(out)


def build() -> str:
    return render(load())


def write(source: str) -> None:
    GENERATED.write_text(source)
