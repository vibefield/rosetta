// The canonical patch, applied the same way to visible rows, held pages and anchors: the TypeScript client's
// `patchRow` and `retainHydratedWorkflowRun` (`clients/ts/client.ts`), over whole-JSON rows.

/// The row after `mutation`, or the row unchanged (nil stays nil but for a `message`, which carries a whole row).
func patchRow(_ row: TimelineEntry?, _ mutation: Mutation) -> TimelineEntry? {
  switch mutation {
  case .message(let incoming):
    guard let row, row.body["type"] == .string("tool_use"), incoming.body["type"] == .string("tool_use"),
      let tool = row.body["tool"], let incomingTool = incoming.body["tool"]
    else { return incoming }
    return incoming.with("body", replacing(incoming.body, "tool", retainHydratedWorkflowRun(tool, incomingTool)))
  case .toolUpdated(_, _, let tool, _):
    guard let row, row.body["type"] == .string("tool_use") else { return row }
    let current = row.body["tool"] ?? .null
    return row.with("body", replacing(row.body, "tool", retainHydratedWorkflowRun(current, tool)))
  case .usage(_, _, let ts, let model, let tokens, let cost):
    guard let row else { return nil }
    // The read-time cost belongs to the replaced usage: never keep the old (a nil cost removes it).
    return row.with("usage", tokens)
      .with("model", model.map(JSONValue.string) ?? .null)
      .with("at", ts.map(JSONValue.int) ?? .null)
      .with("cost", cost)
  default:
    return row
  }
}

/// `run` is a query-time join and therefore absent from row-level mutations. Keep it only while both canonical
/// actions still identify the same run. A replacement snapshot starts from an empty map and remains authoritative.
func retainHydratedWorkflowRun(_ current: JSONValue, _ incoming: JSONValue) -> JSONValue {
  guard let before = current["action"], let after = incoming["action"],
    before["type"] == .string("workflow"), after["type"] == .string("workflow"),
    let run = before["run"], !run.isNull, after["run"] == .null,
    let runId = before["run_id"], !runId.isNull, after["run_id"] == runId
  else { return incoming }
  return replacing(incoming, "action", replacing(after, "run", run))
}

/// An object with one member replaced; anything else unchanged.
private func replacing(_ value: JSONValue, _ key: String, _ member: JSONValue) -> JSONValue {
  guard case .object(var fields) = value else { return value }
  fields[key] = member
  return .object(fields)
}
