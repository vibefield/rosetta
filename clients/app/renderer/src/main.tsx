import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles/tokens.css";
import "./styles/room.css";
import "./styles/ingest.css";
import "./styles/transcript.css";
import "./styles/search.css";
// Column ③'s tree: mille's token surface plus the shipped VibeField theme,
// which reads this app's --vf-* tokens live (its published fallbacks only
// carry a host that has none).
import "@vibecook/mille-ui/tokens.css";
import "@vibecook/mille-ui/theme/vibefield.css";

// No StrictMode: mille's port handshake is single-consumer (a MessagePort,
// once claimed, cannot be re-claimed by a double-invoked effect), and the
// embedding guide's own playground makes the same call.
createRoot(document.getElementById("root")!).render(<App />);
