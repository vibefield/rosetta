"""Differential test vectors from genai-prices' own reference calculator.

Run with a Python that can import the `genai_prices` package built from the
vendored revision (see crates/pricing/catalog/genai-prices/SOURCE.toml), e.g.

    uv venv /tmp/gp && uv pip install --python /tmp/gp/bin/python \\
        -e <genai-prices checkout at that revision>/packages/python
    /tmp/gp/bin/python -m tools.pricing.vectors

It prices a fixed set of exclusive-bucket usage shapes for every first-party
catalog model at timestamps that select each dated period, and writes the
expected pico-USD totals to crates/pricing/tests/genai_prices_vectors.json.
The Rust test `differential.rs` must reproduce every one exactly.
"""

from __future__ import annotations

import json
from datetime import datetime, timezone
from decimal import Decimal

import genai_prices  # type: ignore[import-not-found]

from .generate import FIRST_PARTY, ROOT, TOKEN_FIELDS, _load_upstream, _periods, is_plain_id

OUT = ROOT / "crates" / "pricing" / "tests" / "genai_prices_vectors.json"

# (input, cache_read, cache_write, cache_write_1h, output, thinking, web_searches)
SHAPES = {
    "small": (1_234, 5_678, 910, 1_112, 1_314, 1_516, 0),
    "long": (150_000, 200_000, 10_000, 0, 20_000, 5_000, 0),
    "search": (100, 0, 0, 0, 50, 0, 3),
}
# Timestamps chosen to select every dated period in the pinned data at least
# once, plus both sides of the off-peak windows.
STAMPS = [
    "2025-06-01T12:00:00",
    "2026-01-15T12:00:00",
    "2026-03-13T00:00:01",
    "2026-07-29T12:00:00",
    "2026-07-30T00:00:01",
    "2026-08-20T23:59:59",
    "2026-08-21T00:00:01",
    "2026-09-22T02:00:00",
    "2026-09-22T12:00:00",
]


def _ms(stamp: str) -> int:
    return int(datetime.fromisoformat(stamp).replace(tzinfo=timezone.utc).timestamp() * 1000)


def main() -> int:
    upstream, source = _load_upstream()
    providers = {provider["id"]: provider for provider in upstream}
    vectors = []
    mismatched_models = set()
    for provider_id, prefixes in FIRST_PARTY:
        for raw in providers[provider_id]["models"]:
            if prefixes is not None and not raw["id"].startswith(prefixes):
                continue
            if not is_plain_id(raw["id"]):
                continue
            periods = _periods(raw["prices"], raw["id"])
            if not any(any(name in p.prices for name in TOKEN_FIELDS) for p in periods):
                continue
            has_web = all("web_search_call" in p.prices for p in periods)
            stamps = STAMPS if len(periods) > 1 else STAMPS[-1:]
            for stamp in stamps:
                moment = datetime.fromisoformat(stamp).replace(tzinfo=timezone.utc)
                for shape, (inp, read, write, write_1h, out, think, web) in SHAPES.items():
                    usage = genai_prices.Usage(
                        input_tokens=inp + read + write + write_1h,
                        cache_read_tokens=read,
                        cache_write_tokens=write + write_1h,
                        cache_write_1h_tokens=write_1h,
                        output_tokens=out + think,
                        output_reasoning_tokens=think,
                        web_searches=web,
                    )
                    try:
                        result = genai_prices.calc_price(
                            usage,
                            raw["id"],
                            provider_id=provider_id,
                            genai_request_timestamp=moment,
                        )
                    except LookupError:
                        # Its own match clauses do not accept its id.
                        mismatched_models.add((raw["id"], "<none>"))
                        continue
                    if result.model.id != raw["id"]:
                        # genai-prices' own prefix matching picked another model.
                        mismatched_models.add((raw["id"], result.model.id))
                        continue
                    pico = result.total_price * Decimal(10) ** 12
                    if pico != pico.to_integral_value():
                        raise SystemExit(f"{raw['id']}: non-integral pico total {pico}")
                    # genai-prices charges a unit without a rate nothing;
                    # Rosetta reports the record as unpriced instead.
                    price = result.model_price
                    expect: int | str = int(pico)
                    if out + think and price.output_mtok is None and price.output_reasoning_mtok is None:
                        expect = "missing_rate:output"
                    elif (inp or read or write or write_1h) and price.input_mtok is None:
                        expect = "missing_rate:input"
                    elif web and not has_web:
                        expect = "missing_rate:web_search"
                    vectors.append(
                        {
                            "model": raw["id"],
                            "at_ms": _ms(stamp),
                            "usage": [inp, read, write, write_1h, out, think, web],
                            "expect": expect,
                        }
                    )
    OUT.write_text(
        json.dumps(
            {
                "source": {"revision": source["revision"], "sha256": source["sha256"]},
                "genai_prices_version": genai_prices.__version__,
                "fields": ["input", "cache_read", "cache_write", "cache_write_1h", "output", "thinking", "web_searches"],
                "self_mismatched_models": sorted(f"{a} -> {b}" for a, b in mismatched_models),
                "vectors": vectors,
            },
            separators=(",", ":"),
        )
        + "\n"
    )
    print(f"wrote {len(vectors)} vectors to {OUT.relative_to(ROOT)}")
    if mismatched_models:
        print(f"genai-prices resolved {len(mismatched_models)} model ids to a different model (skipped)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
