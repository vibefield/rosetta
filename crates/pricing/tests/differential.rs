//! Differential proof against genai-prices' own reference calculator
//! (`tools/pricing/vectors.py` at the vendored revision): every first-party
//! model, every dated period, cached/1-hour/reasoning/long-context/search
//! shapes. Rosetta's integer calculator must reproduce each total exactly.

use rosetta_pricing::{Request, Unit, Unpriced, list_cost, resolve, sources};
use serde_json::Value;

#[test]
fn every_reference_vector_prices_identically() {
    let raw = include_str!("genai_prices_vectors.json");
    let document: Value = serde_json::from_str(raw).expect("vectors parse");
    let revision = document["source"]["revision"].as_str().unwrap();
    assert_eq!(
        sources()[0].revision,
        revision,
        "vectors were generated from a different upstream revision; rerun tools.pricing.vectors"
    );
    let vectors = document["vectors"].as_array().unwrap();
    assert!(vectors.len() > 1_000, "the vector set covers the catalog");
    let mut compared = 0;
    for vector in vectors {
        let model = vector["model"].as_str().unwrap();
        assert_eq!(
            resolve(model).map(|entry| entry.id),
            Some(model),
            "catalog id {model} resolves to itself"
        );
        let usage: Vec<i64> = vector["usage"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_i64().unwrap())
            .collect();
        let request = Request {
            model,
            at_ms: vector["at_ms"].as_i64(),
            input: usage[0],
            cache_read: usage[1],
            cache_write: usage[2],
            cache_write_1h: usage[3],
            output: usage[4],
            thinking: usage[5],
            web_searches: usage[6],
            ..Request::default()
        };
        let expected = match &vector["expect"] {
            Value::Number(pico) => Ok(pico.as_i64().unwrap()),
            // genai-prices charges a unit without a rate nothing; Rosetta
            // reports the record as unpriced, never as free.
            Value::String(reason) => Err(Unpriced::MissingRate(match reason.as_str() {
                "missing_rate:input" => Unit::Input,
                "missing_rate:output" => Unit::Output,
                "missing_rate:web_search" => Unit::WebSearch,
                other => panic!("unknown expectation {other}"),
            })),
            other => panic!("bad expectation {other}"),
        };
        assert_eq!(list_cost(&request), expected, "{model} {vector}");
        compared += usize::from(expected.is_ok());
    }
    assert!(compared > 700);
}
