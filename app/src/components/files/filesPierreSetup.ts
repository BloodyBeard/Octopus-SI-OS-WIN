// The regular renderer must retain ownership of the shared custom element.
// Files owns its shadow stylesheet separately, as the original Files host did.
import "@pierre/diffs";
import { registerCustomTheme, wrapCoreCSS } from "@octopus/files-diffs";
import { registerOctopusDiffThemes } from "../settings/octopusDiffThemes";

registerOctopusDiffThemes(registerCustomTheme);

const stylesheets = new WeakMap<Document, CSSStyleSheet>();
export function adoptFilesStyles(host: HTMLElement): void {
  const root = host.shadowRoot;
  if (!root) throw new Error("The Files renderer requires an open shadow root");
  let sheet = stylesheets.get(host.ownerDocument);
  if (!sheet) {
    sheet = new CSSStyleSheet();
    sheet.replaceSync(wrapCoreCSS(""));
    stylesheets.set(host.ownerDocument, sheet);
  }
  if (root.adoptedStyleSheets.length !== 1 || root.adoptedStyleSheets[0] !== sheet) {
    root.adoptedStyleSheets = [sheet];
  }
}
