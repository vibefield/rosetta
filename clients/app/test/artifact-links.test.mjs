import assert from "node:assert/strict";
import test from "node:test";
import { publishedArtifactUrl } from "../../room/src/components/artifacts/preview.ts";

test("published artifacts allow recorded HTTPS links and reject active or credential-bearing URLs", () => {
  assert.equal(publishedArtifactUrl("https://claude.ai/artifact/review"), "https://claude.ai/artifact/review");
  for (const url of [null, "javascript:alert(1)", "data:text/html,<script>", "file:///tmp/review.html", "https://", "https://user:secret@example.com/review", "https://example.com/\nreview"]) {
    assert.equal(publishedArtifactUrl(url), null);
  }
});
