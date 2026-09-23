from __future__ import annotations

import unittest
from decimal import Decimal

from tools.pricing import generate


class GenerateTests(unittest.TestCase):
    def test_committed_catalog_is_current(self) -> None:
        self.assertEqual(generate.GENERATED.read_text(), generate.build())

    def test_prices_convert_exactly_or_fail(self) -> None:
        self.assertEqual(generate._exact(Decimal("3.75"), generate.MTOK, "t"), 3_750_000)
        self.assertEqual(generate._exact(Decimal("0.0375"), generate.MTOK, "t"), 37_500)
        with self.assertRaises(generate.PricingError):
            generate._exact(Decimal("0.0000001"), generate.MTOK, "t")
        with self.assertRaises(generate.PricingError):
            generate._exact(0.075, generate.MTOK, "t")

    def test_prefix_literals_are_exact_aliases_only(self) -> None:
        clause = {"or": [{"starts_with": "claude-opus-5"}, {"regex": "^x$"}, {"contains": "opus"}]}
        self.assertEqual(generate._match_aliases(clause), ["claude-opus-5"])
        self.assertFalse(generate.is_plain_id("claude-sonnet-4@"))
        self.assertFalse(generate.is_plain_id("ft:gpt-3.5-turbo-"))
        self.assertTrue(generate.is_plain_id("gpt-5.6-sol"))

    def test_constraints_and_tiers(self) -> None:
        self.assertEqual(generate._constraint({"start_date": "1970-01-11"}, "t"), ("from_day", 10))
        self.assertEqual(
            generate._constraint({"start_time": "16:30:00Z", "end_time": "00:30:00+08:00"}, "t"),
            ("time_of_day", (59_400, 59_400)),
        )
        rate = generate._rate({"base": Decimal("2.5"), "tiers": [{"start": 271999, "price": Decimal("5")}]}, generate.MTOK, "t")
        self.assertEqual(rate, generate.Rate(2_500_000, ((271_999, 5_000_000),)))
        with self.assertRaises(generate.PricingError):
            generate._constraint({"start_date": "2026-01-01", "start_time": "00:00:00Z"}, "t")

    def test_catalog_invariants(self) -> None:
        catalog = generate.load()
        by_alias = {alias: catalog.models[index] for alias, index in catalog.aliases.items()}
        self.assertEqual(by_alias["claude-opus-5"].id, "claude-opus-5")
        self.assertNotIn("claude-opus-5-5", by_alias)
        self.assertEqual(by_alias["k3"].id, "kimi-k3")
        # Vertex's partner-model Claude entries never shadow Anthropic's own.
        self.assertEqual(by_alias["claude-opus-4-6"].provider, "anthropic")
        self.assertTrue(all(rule["source"] for rule in catalog.tier_rules))
        self.assertTrue(catalog.version.startswith("genai-prices@"))


if __name__ == "__main__":
    unittest.main()
