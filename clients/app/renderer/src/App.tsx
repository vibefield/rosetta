// rosetta's app: the History room's reference host. It owns the one client,
// the desktop bridge and the window; the room reads all of them through the
// ports this app hands it.
import { HistoryRoom } from "./HistoryRoom";
import type { RoomPorts } from "./ports";

export default function App({ ports }: { readonly ports: RoomPorts }) {
  return <HistoryRoom {...ports} />;
}
