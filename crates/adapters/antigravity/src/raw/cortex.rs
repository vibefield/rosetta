//! Typed views of the conversation-store protobuf messages the adapter reads.
//!
//! The conversation store `conversations/<id>.db` keeps its payloads as
//! protobuf blobs. The canonical ground truth declares, per message, the
//! field numbers the writer's own descriptor set carries (`agy` 1.2.9
//! `FileDescriptorSet`, stream `conversation-db`, facet `protobuf`); the
//! `FIELDS` constants below are that declaration and `spec` tests hold them
//! equal to the canonical. A number on the wire that is not declared is
//! drift, exactly like an unknown JSON key. Only the fields the usage rules
//! need are decoded; every other declared field is recognized and skipped.
//!
//! Proto3 scalars have no presence: a zero is not written, so an absent
//! numeric field decodes as `0` and an absent string as `None`. That is the
//! wire's own semantics, not an adapter guess.

use rosetta_adapter_support::Drift;
use rosetta_core::rows::Ms;

use super::wire::{WireError, WireValue, read_fields, repeated_varints};

/// `google.protobuf.Timestamp`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timestamp {
    pub seconds: i64,
    pub nanos: i32,
    pub unknown_fields: Vec<u32>,
}

impl Timestamp {
    pub const NAME: &'static str = "google.protobuf.Timestamp";
    pub const FIELDS: &'static [u32] = &[1, 2];

    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut out = Self::default();
        for field in read_fields(bytes)? {
            match field.number {
                1 => out.seconds = signed(field.value),
                2 => out.nanos = signed(field.value) as i32,
                other => out.unknown_fields.push(other),
            }
        }
        finish(&mut out.unknown_fields);
        Ok(out)
    }

    /// Milliseconds since the Unix epoch, floored. `None` only on overflow.
    pub fn epoch_ms(&self) -> Option<Ms> {
        self.seconds
            .checked_mul(1_000)?
            .checked_add(i64::from(self.nanos).div_euclid(1_000_000))
    }
}

/// `exa.codeium_common_pb.ModelUsageStats` — one model request as the
/// agent's backend reported it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelUsageStats {
    /// `Model` enum value (`#1`); the names are placeholders in the shipped
    /// descriptors, so this never resolves to a model string.
    pub model: u64,
    /// `#2`: prompt tokens that were neither cache reads nor cache writes.
    pub input_tokens: u64,
    /// `#3`: all generated tokens, `thinking_output_tokens` included.
    pub output_tokens: u64,
    /// `#4`, deprecated in the descriptor and zero in every observed record.
    pub cache_write_tokens: u64,
    /// `#5`: prompt tokens served from the provider cache.
    pub cache_read_tokens: u64,
    /// `APIProvider` enum value (`#6`).
    pub api_provider: u64,
    /// `#9`: reasoning tokens, a subset of `output_tokens`.
    pub thinking_output_tokens: u64,
    /// `#10`: visible response tokens, the rest of `output_tokens`.
    pub response_output_tokens: u64,
    /// `#13`: the provider's service tier string; absent when empty.
    pub service_tier: Option<String>,
    pub unknown_fields: Vec<u32>,
}

impl ModelUsageStats {
    pub const NAME: &'static str = "exa.codeium_common_pb.ModelUsageStats";
    pub const FIELDS: &'static [u32] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17];

    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut out = Self::default();
        for field in read_fields(bytes)? {
            match field.number {
                1 => out.model = unsigned(field.value),
                2 => out.input_tokens = unsigned(field.value),
                3 => out.output_tokens = unsigned(field.value),
                4 => out.cache_write_tokens = unsigned(field.value),
                5 => out.cache_read_tokens = unsigned(field.value),
                6 => out.api_provider = unsigned(field.value),
                9 => out.thinking_output_tokens = unsigned(field.value),
                10 => out.response_output_tokens = unsigned(field.value),
                13 => out.service_tier = string(field.value),
                declared if Self::FIELDS.contains(&declared) => {}
                other => out.unknown_fields.push(other),
            }
        }
        finish(&mut out.unknown_fields);
        Ok(out)
    }

    /// No token count at all: a message that names a model and provider but
    /// accounts nothing is not evidence of a request.
    pub fn is_empty(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_write_tokens == 0
            && self.cache_read_tokens == 0
            && self.thinking_output_tokens == 0
            && self.response_output_tokens == 0
    }
}

/// `exa.cortex_pb.RetryInfo` — one attempt of a generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetryInfo {
    pub usage: Option<ModelUsageStats>,
    /// `RetryReason` enum value (`#5`).
    pub retry_reason: u64,
    pub unknown_fields: Vec<u32>,
}

impl RetryInfo {
    pub const NAME: &'static str = "exa.cortex_pb.RetryInfo";
    pub const FIELDS: &'static [u32] = &[1, 2, 3, 4, 5, 6, 7];

    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut out = Self::default();
        for field in read_fields(bytes)? {
            match field.number {
                2 => out.usage = nested(field.value, ModelUsageStats::decode)?,
                5 => out.retry_reason = unsigned(field.value),
                declared if Self::FIELDS.contains(&declared) => {}
                other => out.unknown_fields.push(other),
            }
        }
        finish(&mut out.unknown_fields);
        Ok(out)
    }
}

/// `exa.cortex_pb.CortexStepMetadata` — the `steps.metadata` column: one
/// row per cascade step, carrying the request grain of usage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepMetadata {
    pub created_at: Option<Timestamp>,
    /// `#9`: the usage of this step's own model request(s).
    pub model_usage: Option<ModelUsageStats>,
    /// `Model` enum value (`#11`); placeholder names only.
    pub generator_model: u64,
    /// `#28`: the attempts behind `model_usage`, when the writer lists them.
    pub retry_infos: Vec<RetryInfo>,
    pub unknown_fields: Vec<u32>,
}

impl StepMetadata {
    pub const NAME: &'static str = "exa.cortex_pb.CortexStepMetadata";
    pub const FIELDS: &'static [u32] = &[
        1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
        27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39,
    ];

    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut out = Self::default();
        for field in read_fields(bytes)? {
            match field.number {
                1 => out.created_at = nested(field.value, Timestamp::decode)?,
                9 => out.model_usage = nested(field.value, ModelUsageStats::decode)?,
                11 => out.generator_model = unsigned(field.value),
                28 => out
                    .retry_infos
                    .extend(nested(field.value, RetryInfo::decode)?),
                declared if Self::FIELDS.contains(&declared) => {}
                other => out.unknown_fields.push(other),
            }
        }
        finish(&mut out.unknown_fields);
        Ok(out)
    }
}

/// `exa.cortex_pb.ChatModelMetadata` — one generation's model call, inside
/// `CortexStepGeneratorMetadata`. It carries the only model *string* the
/// store persists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatModelMetadata {
    /// `Model` enum value (`#3`).
    pub model: u64,
    /// `#4`: the sum over this generation's attempts.
    pub usage: Option<ModelUsageStats>,
    /// `#17`: one entry per attempt.
    pub retry_infos: Vec<RetryInfo>,
    /// `#19`: the served model id, e.g. `gemini-3.7-flash`; absent when the
    /// backend did not report one (the mock never does).
    pub response_model: Option<String>,
    /// `#21`: a display label, never a catalog id.
    pub model_display_name: Option<String>,
    pub unknown_fields: Vec<u32>,
}

impl ChatModelMetadata {
    pub const NAME: &'static str = "exa.cortex_pb.ChatModelMetadata";
    pub const FIELDS: &'static [u32] = &[
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
    ];

    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut out = Self::default();
        for field in read_fields(bytes)? {
            match field.number {
                3 => out.model = unsigned(field.value),
                4 => out.usage = nested(field.value, ModelUsageStats::decode)?,
                17 => out
                    .retry_infos
                    .extend(nested(field.value, RetryInfo::decode)?),
                19 => out.response_model = string(field.value),
                21 => out.model_display_name = string(field.value),
                declared if Self::FIELDS.contains(&declared) => {}
                other => out.unknown_fields.push(other),
            }
        }
        finish(&mut out.unknown_fields);
        Ok(out)
    }
}

/// `exa.cortex_pb.CortexStepGeneratorMetadata` — the `gen_metadata.data`
/// column: one row per generation, naming the steps it produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeneratorMetadata {
    /// `#2`: the `steps.idx` values this generation wrote.
    pub step_indices: Vec<u32>,
    /// `#1`: present for a model generation; an `injected` (`#7`) generation
    /// has no model call and no usage.
    pub chat_model: Option<ChatModelMetadata>,
    pub unknown_fields: Vec<u32>,
}

impl GeneratorMetadata {
    pub const NAME: &'static str = "exa.cortex_pb.CortexStepGeneratorMetadata";
    pub const FIELDS: &'static [u32] = &[1, 2, 3, 4, 5, 7, 8, 9, 10, 11];

    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut out = Self::default();
        for field in read_fields(bytes)? {
            match field.number {
                1 => out.chat_model = nested(field.value, ChatModelMetadata::decode)?,
                2 => out.step_indices.extend(
                    repeated_varints(field.value)?
                        .into_iter()
                        .filter_map(|index| u32::try_from(index).ok()),
                ),
                declared if Self::FIELDS.contains(&declared) => {}
                other => out.unknown_fields.push(other),
            }
        }
        finish(&mut out.unknown_fields);
        Ok(out)
    }
}

impl Drift for Timestamp {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_numbers(&self.unknown_fields, prefix, out);
    }
}

impl Drift for ModelUsageStats {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_numbers(&self.unknown_fields, prefix, out);
    }
}

impl Drift for RetryInfo {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_numbers(&self.unknown_fields, prefix, out);
        if let Some(usage) = &self.usage {
            usage.drift(&format!("{prefix}.usage"), out);
        }
    }
}

impl Drift for StepMetadata {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_numbers(&self.unknown_fields, prefix, out);
        if let Some(created_at) = &self.created_at {
            created_at.drift(&format!("{prefix}.created_at"), out);
        }
        if let Some(usage) = &self.model_usage {
            usage.drift(&format!("{prefix}.model_usage"), out);
        }
        for retry in &self.retry_infos {
            retry.drift(&format!("{prefix}.retry_infos[]"), out);
        }
    }
}

impl Drift for ChatModelMetadata {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_numbers(&self.unknown_fields, prefix, out);
        if let Some(usage) = &self.usage {
            usage.drift(&format!("{prefix}.usage"), out);
        }
        for retry in &self.retry_infos {
            retry.drift(&format!("{prefix}.retry_infos[]"), out);
        }
    }
}

impl Drift for GeneratorMetadata {
    fn drift(&self, prefix: &str, out: &mut Vec<String>) {
        note_numbers(&self.unknown_fields, prefix, out);
        if let Some(chat_model) = &self.chat_model {
            chat_model.drift(&format!("{prefix}.chat_model"), out);
        }
    }
}

fn note_numbers(numbers: &[u32], prefix: &str, out: &mut Vec<String>) {
    for number in numbers {
        out.push(format!("{prefix}.#{number}"));
    }
}

fn finish(unknown: &mut Vec<u32>) {
    unknown.sort_unstable();
    unknown.dedup();
}

fn unsigned(value: WireValue<'_>) -> u64 {
    value.as_u64().unwrap_or(0)
}

/// `int32`/`int64` are two's-complement varints.
fn signed(value: WireValue<'_>) -> i64 {
    value.as_u64().unwrap_or(0) as i64
}

fn string(value: WireValue<'_>) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Decode a length-delimited sub-message; a scalar in a message slot is not
/// framable and counts as absent.
fn nested<T>(
    value: WireValue<'_>,
    decode: impl FnOnce(&[u8]) -> Result<T, WireError>,
) -> Result<Option<T>, WireError> {
    value.as_bytes().map(decode).transpose()
}

#[cfg(test)]
mod tests {
    use super::super::wire::encode::{
        field_bytes, field_str, field_varint, message, packed_varints,
    };
    use super::{
        ChatModelMetadata, GeneratorMetadata, ModelUsageStats, RetryInfo, StepMetadata, Timestamp,
    };
    use rosetta_adapter_support::Drift;

    fn usage(input: u64, thinking: u64, response: u64, cache_read: u64) -> Vec<u8> {
        message(&[
            field_varint(1, 1298),
            field_varint(2, input),
            field_varint(3, thinking + response),
            field_varint(5, cache_read),
            field_varint(6, 24),
            field_varint(9, thinking),
            field_varint(10, response),
        ])
    }

    #[test]
    fn step_metadata_decodes_the_request_grain() {
        let created = message(&[field_varint(1, 1_787_634_643), field_varint(2, 907_000_000)]);
        let bytes = message(&[
            field_bytes(1, &created),
            field_varint(3, 2),
            field_bytes(9, &usage(14_181, 145, 51, 0)),
            field_varint(11, 1298),
            field_str(12, "fe9b3d0f"),
            field_bytes(28, &message(&[field_bytes(2, &usage(14_181, 145, 51, 0))])),
        ]);
        let step = StepMetadata::decode(&bytes).unwrap();
        assert_eq!(
            step.created_at.as_ref().unwrap().epoch_ms(),
            Some(1_787_634_643_907)
        );
        let usage = step.model_usage.as_ref().unwrap();
        assert_eq!(
            (
                usage.input_tokens,
                usage.output_tokens,
                usage.thinking_output_tokens,
                usage.response_output_tokens,
                usage.cache_read_tokens,
            ),
            (14_181, 196, 145, 51, 0)
        );
        assert_eq!(usage.service_tier, None);
        assert_eq!(step.generator_model, 1298);
        assert_eq!(step.retry_infos.len(), 1);
        assert!(step.drift_paths().is_empty());
    }

    #[test]
    fn generator_metadata_names_its_steps_and_served_model() {
        let chat = message(&[
            field_str(1, "system prompt"),
            field_varint(3, 1298),
            field_bytes(4, &usage(256, 0, 64, 0)),
            field_bytes(17, &message(&[field_bytes(2, &usage(128, 0, 32, 0))])),
            field_bytes(
                17,
                &message(&[field_bytes(2, &usage(128, 0, 32, 0)), field_varint(5, 2)]),
            ),
            field_str(19, "gemini-3.7-flash"),
            field_str(21, "Gemini 3.7 Flash"),
        ]);
        let bytes = message(&[
            field_bytes(1, &chat),
            packed_varints(2, &[1, 2]),
            field_str(4, "execution"),
        ]);
        let generation = GeneratorMetadata::decode(&bytes).unwrap();
        assert_eq!(generation.step_indices, [1, 2]);
        let chat = generation.chat_model.as_ref().unwrap();
        assert_eq!(chat.response_model.as_deref(), Some("gemini-3.7-flash"));
        assert_eq!(chat.model_display_name.as_deref(), Some("Gemini 3.7 Flash"));
        assert_eq!(chat.usage.as_ref().unwrap().input_tokens, 256);
        assert_eq!(chat.retry_infos.len(), 2);
        assert_eq!(chat.retry_infos[1].retry_reason, 2);
        assert!(generation.drift_paths().is_empty());

        let unpacked = message(&[field_varint(2, 7), field_varint(2, 9)]);
        assert_eq!(
            GeneratorMetadata::decode(&unpacked).unwrap().step_indices,
            [7, 9]
        );
    }

    #[test]
    fn undeclared_numbers_are_drift_paths_and_declared_ones_are_silent() {
        let bytes = message(&[
            field_bytes(9, &message(&[field_varint(2, 5), field_varint(99, 1)])),
            field_varint(39, 1),
            field_str(40, "future"),
            field_bytes(1, &message(&[field_varint(1, 1), field_varint(3, 4)])),
        ]);
        let step = StepMetadata::decode(&bytes).unwrap();
        assert_eq!(
            step.drift_paths(),
            [".#40", ".created_at.#3", ".model_usage.#99"]
        );
        assert_eq!(step.unknown_fields, [40]);
    }

    #[test]
    fn absent_scalars_decode_as_zero_and_empty_strings_as_absent() {
        let usage =
            ModelUsageStats::decode(&message(&[field_str(13, ""), field_varint(6, 24)])).unwrap();
        assert!(usage.is_empty());
        assert_eq!(usage.service_tier, None);
        assert_eq!(usage.api_provider, 24);
        let usage =
            ModelUsageStats::decode(&message(&[field_str(13, "priority"), field_varint(2, 1)]))
                .unwrap();
        assert!(!usage.is_empty());
        assert_eq!(usage.service_tier.as_deref(), Some("priority"));
        assert_eq!(RetryInfo::decode(&[]).unwrap(), RetryInfo::default());
        assert_eq!(ChatModelMetadata::decode(&[]).unwrap().response_model, None);
    }

    #[test]
    fn timestamps_floor_to_milliseconds_and_flag_overflow() {
        let stamp = Timestamp::decode(&message(&[
            field_varint(1, 1_700_000_000),
            field_varint(2, 999_999),
        ]))
        .unwrap();
        assert_eq!(stamp.epoch_ms(), Some(1_700_000_000_000));
        let negative = Timestamp {
            seconds: -1,
            nanos: 500_000_000,
            unknown_fields: Vec::new(),
        };
        assert_eq!(negative.epoch_ms(), Some(-500));
        let overflow = Timestamp {
            seconds: i64::MAX,
            nanos: 0,
            unknown_fields: Vec::new(),
        };
        assert_eq!(overflow.epoch_ms(), None);
    }

    #[test]
    fn a_corrupt_nested_message_fails_the_whole_decode() {
        let bytes = message(&[field_bytes(9, &[0x80])]);
        assert!(StepMetadata::decode(&bytes).is_err());
        let bytes = message(&[field_bytes(1, &message(&[field_bytes(4, &[0x08])]))]);
        assert!(GeneratorMetadata::decode(&bytes).is_err());
    }
}
