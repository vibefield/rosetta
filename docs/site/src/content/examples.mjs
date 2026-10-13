// Shared, copyable examples. SearchRequest represents absent wire parameters
// with explicit nulls; omitting those fields would not compile against the SDK.
export const searchExample = `const result = await client.search({
  q: "authentication",
  limit: 20,
  session: null,
  sessions: null,
  prefix: false,
  runtime: null,
  agent: null,
  project: null,
  kind: null,
  is_error: null,
  after_ts: null,
  before_ts: null,
});

console.log(result.state.readiness, result.hits);`;
