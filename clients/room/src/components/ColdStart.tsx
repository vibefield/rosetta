import type { IngestActivity } from "../lib/ingest-activity";
import { IngestStatus } from "./IngestStatus";

export function ColdStartPanel({ activity }: { readonly activity: IngestActivity | null }) {
  return (
    <div className="ah-empty ah-cold-start">
      <strong>Bringing your history together</strong>
      <p>Your projects will appear here as we discover your conversations.</p>
      <IngestStatus activity={activity} />
    </div>
  );
}
