import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles/tokens.css";
import "./styles/room.css";

// DESIGN.md §10 — the class and the attribute are stamped together, and the
// room follows the system with no toggle of its own (for now).
const scheme = window.matchMedia("(prefers-color-scheme: dark)");
function applyTheme() {
  document.documentElement.classList.toggle("dark", scheme.matches);
  document.documentElement.setAttribute("data-theme", scheme.matches ? "dark" : "light");
}
scheme.addEventListener("change", applyTheme);
applyTheme();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
