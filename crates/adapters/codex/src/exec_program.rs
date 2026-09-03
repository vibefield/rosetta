//! Conservative reader for Codex's `functions.exec` JavaScript envelope.
//!
//! Recent Codex writers persist the outer executor as the native tool call and
//! put the actual tool calls in its JavaScript body. This is not a general
//! JavaScript parser: it recognizes only direct `tools.name(...)` call sites
//! and statically-authored scalar arguments. Anything dynamic remains an
//! orchestration action in `tool.rs`.

const CALL_LIMIT: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmbeddedCall {
    pub name: String,
    pub start: usize,
    pub argument: String,
}

pub(crate) fn tool_calls(program: &str) -> Vec<EmbeddedCall> {
    let bytes = program.as_bytes();
    let mut calls = Vec::new();
    let mut index = 0;
    while index < bytes.len() && calls.len() < CALL_LIMIT {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                index = skip_quoted(bytes, index);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index + 2);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index + 2);
                continue;
            }
            _ => {}
        }

        if bytes[index..].starts_with(b"tools.") && identifier_boundary(bytes, index) {
            let name_start = index + "tools.".len();
            let mut name_end = name_start;
            while name_end < bytes.len() && is_identifier_byte(bytes[name_end]) {
                name_end += 1;
            }
            let mut open = name_end;
            while open < bytes.len() && bytes[open].is_ascii_whitespace() {
                open += 1;
            }
            if name_end > name_start
                && bytes.get(open) == Some(&b'(')
                && let Some(close) = matching_paren(bytes, open)
            {
                calls.push(EmbeddedCall {
                    name: program[name_start..name_end].to_owned(),
                    start: index,
                    argument: program[open + 1..close].trim().to_owned(),
                });
            }
        }
        index += 1;
    }
    calls
}

/// Statically-authored calls that are direct elements of a literal
/// `Promise.all([ ... ])`. Unlike a call site inside `items.map(...)`, each of
/// these expressions executes exactly once, so the adapter may safely expose
/// the real child actions instead of an opaque orchestration row.
pub(crate) fn literal_parallel_calls(program: &str) -> Option<Vec<EmbeddedCall>> {
    let bytes = program.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                index = skip_quoted(bytes, index);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index + 2);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index + 2);
                continue;
            }
            _ => {}
        }

        if bytes[index..].starts_with(b"Promise.all") && identifier_boundary(bytes, index) {
            let mut open = skip_space_and_comments(bytes, index + "Promise.all".len());
            if bytes.get(open) != Some(&b'(') {
                index += 1;
                continue;
            }
            open = skip_space_and_comments(bytes, open + 1);
            if bytes.get(open) != Some(&b'[') {
                index += 1;
                continue;
            }
            let close = matching_bracket(bytes, open)?;
            return direct_array_calls(program, open + 1, close);
        }
        index += 1;
    }
    None
}

fn direct_array_calls(program: &str, start: usize, end: usize) -> Option<Vec<EmbeddedCall>> {
    let body = &program[start..end];
    let bytes = body.as_bytes();
    let mut calls = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        index = skip_space_commas_and_comments(bytes, index);
        if index >= bytes.len() {
            break;
        }
        let expression_end = expression_end(bytes, index);
        let expression = body[index..expression_end].trim();
        if expression.is_empty() {
            index = expression_end.saturating_add(1);
            continue;
        }
        let local = tool_calls(expression);
        let [call] = local.as_slice() else {
            return None;
        };
        let prefix = expression[..call.start].trim();
        if !prefix.is_empty() && prefix != "await" {
            return None;
        }
        let name_end = call.start + "tools.".len() + call.name.len();
        let call_open = skip_space_and_comments(expression.as_bytes(), name_end);
        let call_close = matching_paren(expression.as_bytes(), call_open)?;
        if !expression[call_close + 1..].trim().is_empty() {
            return None;
        }
        let expression_offset = body[index..expression_end]
            .find(expression)
            .map(|offset| start + index + offset)?;
        calls.push(EmbeddedCall {
            name: call.name.clone(),
            start: expression_offset + call.start,
            argument: call.argument.clone(),
        });
        index = expression_end;
        if bytes.get(index) == Some(&b',') {
            index += 1;
        }
    }
    (calls.len() > 1).then_some(calls)
}

pub(crate) fn property_string(program: &str, call: &EmbeddedCall, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        object_property(&call.argument, key)
            .and_then(|value| resolve_string(program, call.start, value))
    })
}

pub(crate) fn nested_property_string(
    program: &str,
    call: &EmbeddedCall,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        property_anywhere(&call.argument, key)
            .and_then(|value| resolve_string(program, call.start, value))
    })
}

pub(crate) fn argument_string(program: &str, call: &EmbeddedCall) -> Option<String> {
    let value = first_expression(&call.argument);
    resolve_string(program, call.start, value)
}

pub(crate) fn property_u64(call: &EmbeddedCall, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| object_property(&call.argument, key))
        .and_then(|value| value.trim().parse().ok())
}

pub(crate) fn property_bool(call: &EmbeddedCall, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| object_property(&call.argument, key))
        .and_then(|value| match value.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        })
}

pub(crate) fn nested_property_count(call: &EmbeddedCall, key: &str) -> usize {
    property_positions(&call.argument, key).count()
}

pub(crate) fn has_property(call: &EmbeddedCall, key: &str) -> bool {
    property_anywhere(&call.argument, key).is_some()
}

fn identifier_boundary(bytes: &[u8], start: usize) -> bool {
    start == 0 || !is_identifier_byte(bytes[start - 1]) && bytes[start - 1] != b'.'
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

fn skip_quoted(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == quote {
            return index + 1;
        } else {
            index += 1;
        }
    }
    bytes.len()
}

fn skip_line_comment(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

fn skip_block_comment(bytes: &[u8], mut index: usize) -> usize {
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}

fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = open;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                index = skip_quoted(bytes, index);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index + 2);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index + 2);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn matching_bracket(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = open;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                index = skip_quoted(bytes, index);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index + 2);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index + 2);
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn object_property<'a>(source: &'a str, wanted: &str) -> Option<&'a str> {
    let bytes = source.as_bytes();
    let mut index = skip_space_and_comments(bytes, 0);
    if bytes.get(index) != Some(&b'{') {
        return None;
    }
    index += 1;
    loop {
        index = skip_space_commas_and_comments(bytes, index);
        if index >= bytes.len() || bytes[index] == b'}' {
            return None;
        }

        let (key, after_key) = if matches!(bytes[index], b'\'' | b'"') {
            let end = skip_quoted(bytes, index);
            let value = decode_string(&source[index..end])?;
            (value, end)
        } else if is_identifier_byte(bytes[index]) {
            let start = index;
            while index < bytes.len() && is_identifier_byte(bytes[index]) {
                index += 1;
            }
            (source[start..index].to_owned(), index)
        } else {
            index = expression_end(bytes, index);
            if bytes.get(index) == Some(&b',') {
                index += 1;
            }
            continue;
        };

        index = skip_space_and_comments(bytes, after_key);
        if bytes.get(index) != Some(&b':') {
            index = expression_end(bytes, index);
            if bytes.get(index) == Some(&b',') {
                index += 1;
            }
            continue;
        }
        let value_start = skip_space_and_comments(bytes, index + 1);
        let value_end = expression_end(bytes, value_start);
        if key == wanted {
            return Some(source[value_start..value_end].trim());
        }
        index = value_end;
        if bytes.get(index) == Some(&b',') {
            index += 1;
        }
    }
}

fn property_anywhere<'a>(source: &'a str, wanted: &str) -> Option<&'a str> {
    property_positions(source, wanted).next()
}

fn property_positions<'a>(source: &'a str, wanted: &str) -> impl Iterator<Item = &'a str> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                index = skip_quoted(bytes, index);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index + 2);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index + 2);
                continue;
            }
            _ => {}
        }
        if is_identifier_byte(bytes[index]) && (index == 0 || !is_identifier_byte(bytes[index - 1]))
        {
            let start = index;
            while index < bytes.len() && is_identifier_byte(bytes[index]) {
                index += 1;
            }
            if &source[start..index] == wanted {
                let colon = skip_space_and_comments(bytes, index);
                if bytes.get(colon) == Some(&b':') {
                    let value_start = skip_space_and_comments(bytes, colon + 1);
                    let value_end = expression_end(bytes, value_start);
                    found.push(source[value_start..value_end].trim());
                }
            }
            continue;
        }
        index += 1;
    }
    found.into_iter()
}

fn first_expression(source: &str) -> &str {
    let end = expression_end(source.as_bytes(), 0);
    source[..end].trim()
}

fn expression_end(bytes: &[u8], mut index: usize) -> usize {
    let mut braces = 0usize;
    let mut brackets = 0usize;
    let mut parens = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                index = skip_quoted(bytes, index);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index + 2);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index + 2);
                continue;
            }
            b'{' => braces += 1,
            b'}' if braces > 0 => braces -= 1,
            b'}' if brackets == 0 && parens == 0 => return index,
            b'[' => brackets += 1,
            b']' if brackets > 0 => brackets -= 1,
            b'(' => parens += 1,
            b')' if parens > 0 => parens -= 1,
            b',' if braces == 0 && brackets == 0 && parens == 0 => return index,
            _ => {}
        }
        index += 1;
    }
    index
}

fn skip_space_and_comments(bytes: &[u8], mut index: usize) -> usize {
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index) == Some(&b'/') && bytes.get(index + 1) == Some(&b'/') {
            index = skip_line_comment(bytes, index + 2);
        } else if bytes.get(index) == Some(&b'/') && bytes.get(index + 1) == Some(&b'*') {
            index = skip_block_comment(bytes, index + 2);
        } else {
            return index;
        }
    }
}

fn skip_space_commas_and_comments(bytes: &[u8], mut index: usize) -> usize {
    loop {
        index = skip_space_and_comments(bytes, index);
        if bytes.get(index) != Some(&b',') {
            return index;
        }
        index += 1;
    }
}

fn resolve_string(program: &str, before: usize, expression: &str) -> Option<String> {
    let expression = expression.trim();
    if matches!(expression.as_bytes().first(), Some(b'\'' | b'"' | b'`')) {
        let end = skip_quoted(expression.as_bytes(), 0);
        if expression[end..].trim().is_empty() {
            return decode_string(&expression[..end]);
        }
        return None;
    }
    if expression.bytes().all(is_identifier_byte) {
        return binding_string(program, before, expression);
    }
    None
}

fn binding_string(program: &str, before: usize, wanted: &str) -> Option<String> {
    let source = &program[..before.min(program.len())];
    let bytes = source.as_bytes();
    let mut candidate = None;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                index = skip_quoted(bytes, index);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index + 2);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index + 2);
                continue;
            }
            _ => {}
        }
        let declaration = ["const", "let", "var"].into_iter().find(|keyword| {
            source[index..].starts_with(keyword)
                && (index == 0 || !is_identifier_byte(bytes[index - 1]))
                && bytes
                    .get(index + keyword.len())
                    .is_none_or(|byte| !is_identifier_byte(*byte))
        });
        let Some(keyword) = declaration else {
            index += 1;
            continue;
        };
        let mut cursor = skip_space_and_comments(bytes, index + keyword.len());
        let name_start = cursor;
        while cursor < bytes.len() && is_identifier_byte(bytes[cursor]) {
            cursor += 1;
        }
        if &source[name_start..cursor] != wanted {
            index = cursor.max(index + 1);
            continue;
        }
        cursor = skip_space_and_comments(bytes, cursor);
        if bytes.get(cursor) != Some(&b'=') {
            index = cursor.max(index + 1);
            continue;
        }
        let value_start = skip_space_and_comments(bytes, cursor + 1);
        if !matches!(bytes.get(value_start), Some(b'\'' | b'"' | b'`')) {
            index = value_start.max(index + 1);
            continue;
        }
        let value_end = skip_quoted(bytes, value_start);
        let trailing = source[value_end..]
            .split_once(';')
            .map_or(source[value_end..].trim(), |(head, _)| head.trim());
        if trailing.is_empty() {
            candidate = decode_string(&source[value_start..value_end]);
        }
        index = value_end;
    }
    candidate
}

fn decode_string(literal: &str) -> Option<String> {
    let quote = *literal.as_bytes().first()?;
    if literal.as_bytes().last() != Some(&quote) {
        return None;
    }
    if quote == b'"' {
        return serde_json::from_str(literal).ok();
    }
    if !matches!(quote, b'\'' | b'`') {
        return None;
    }
    let inner = &literal[1..literal.len() - 1];
    let mut chars = inner.chars().peekable();
    let mut decoded = String::with_capacity(inner.len());
    while let Some(character) = chars.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        let escaped = chars.next()?;
        match escaped {
            '\n' => {}
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            'b' => decoded.push('\u{0008}'),
            'f' => decoded.push('\u{000c}'),
            'v' => decoded.push('\u{000b}'),
            '0' => decoded.push('\0'),
            '\\' | '\'' | '"' | '`' | '/' => decoded.push(escaped),
            other => decoded.push(other),
        }
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_calls_and_ignores_tool_text_inside_strings_and_comments() {
        let program = r#"
            const fake = "tools.apply_patch('not a call')";
            // tools.view_image({path: "ignored.png"})
            const rows = await Promise.all([
              tools.exec_command({cmd:"rg -n 'x(y)' src", workdir:"/work"}),
              tools.write_stdin({session_id:42, chars:""})
            ]);
        "#;
        let calls = tool_calls(program);
        assert_eq!(
            calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>(),
            ["exec_command", "write_stdin"]
        );
        assert_eq!(
            property_string(program, &calls[0], &["cmd"]).as_deref(),
            Some("rg -n 'x(y)' src")
        );
        assert_eq!(property_u64(&calls[1], &["session_id"]), Some(42));
    }

    #[test]
    fn proves_only_direct_literal_parallel_calls() {
        let program = r#"
            const rows = await Promise.all([
              tools.exec_command({cmd:"cargo test"}),
              await tools.exec_command({cmd:"npm test"}),
            ]);
        "#;
        let calls = literal_parallel_calls(program).expect("literal batch");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            property_string(program, &calls[0], &["cmd"]).as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            property_string(program, &calls[1], &["cmd"]).as_deref(),
            Some("npm test")
        );

        assert!(
            literal_parallel_calls(
                r#"await Promise.all(items.map((item) => tools.exec_command({cmd:item})));"#
            )
            .is_none(),
            "a dynamic map is not a proven execution count"
        );
    }

    #[test]
    fn resolves_a_static_patch_binding_without_evaluating_javascript() {
        let program = r#"
            const patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch";
            const result = await tools.apply_patch(patch);
            text(result);
        "#;
        let calls = tool_calls(program);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            argument_string(program, &calls[0]).as_deref(),
            Some("*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch")
        );
    }

    #[test]
    fn reads_nested_fields_and_counts_plan_steps() {
        let program = r#"const p = await tools.update_plan({plan:[
          {step:"read",status:"in_progress"},
          {step:"write",status:"pending"}
        ], explanation:"because"}); text(p);"#;
        let call = &tool_calls(program)[0];
        assert_eq!(nested_property_count(call, "step"), 2);
        assert_eq!(
            nested_property_string(program, call, &["explanation"]).as_deref(),
            Some("because")
        );
    }
}
