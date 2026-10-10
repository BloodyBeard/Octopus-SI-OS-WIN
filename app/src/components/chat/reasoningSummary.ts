export function reasoningSummaryHeading(value: string): string {
  const lines = value.trimEnd().split(/\r?\n/);
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    const line = reasoningVisibleLine(lines[index]?.trim() ?? "");
    if (!line) continue;
    const strong = line.match(/^\*\*(.+)\*\*$/);
    if (strong?.[1]?.trim()) return reasoningSummaryHeading(strong[1].trim()) || strong[1].trim();
    const markdownHeading = line.match(/^#{1,6}[ \t]+(.+?)(?:[ \t]+#+)?$/);
    if (markdownHeading?.[1]?.trim()) return markdownHeading[1].trim();
    return line;
  }
  return "";
}

function reasoningVisibleLine(value: string): string {
  let rest = value;
  while (rest.startsWith("<!--")) {
    const end = rest.indexOf("-->");
    if (end < 0) return "";
    rest = rest.slice(end + 3).trimStart();
  }
  return !rest || "<!--".startsWith(rest) ? "" : rest;
}
