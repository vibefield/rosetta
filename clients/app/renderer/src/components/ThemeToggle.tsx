import { MoonGlyph, SunGlyph } from "@vibefield/rosetta-room";
import { useTheme } from "../lib/theme";

export function ThemeToggle() {
  const { theme, toggleTheme } = useTheme();
  const label = `Switch to ${theme === "dark" ? "light" : "dark"} theme`;
  return (
    <button
      type="button"
      className="ah-iconbtn ah-theme-toggle"
      aria-label={label}
      title={label}
      onClick={toggleTheme}
    >
      {theme === "dark" ? <SunGlyph /> : <MoonGlyph />}
    </button>
  );
}
