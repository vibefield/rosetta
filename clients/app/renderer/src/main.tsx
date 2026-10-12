import { createRoot } from "react-dom/client";
import App from "./App";
import { createRosettaHost } from "./host/rosetta-host";
import "./styles/tokens.css";
import "./styles/app.css";
// The room's own stylesheet, scoped to its root: its sheets and, for column
// ③'s tree, mille's token surface and the VibeField theme, which reads this
// app's --vf-* tokens live.
import "@vibefield/rosetta-room/styles/room.css";

// No StrictMode: mille's port handshake is single-consumer (a MessagePort,
// once claimed, cannot be re-claimed by a double-invoked effect), and the
// embedding guide's own playground makes the same call.
const host = createRosettaHost();
createRoot(document.getElementById("root")!).render(<App host={host} />);
