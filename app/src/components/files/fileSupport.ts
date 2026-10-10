import { sha256 } from "@noble/hashes/sha256";
import { bytesToHex } from "@noble/hashes/utils";
import { EXTENSION_TO_FILE_FORMAT, type VirtualFileMetrics, type FileContents } from "@octopus/files-diffs";

export type FileMetrics = VirtualFileMetrics;

export function fileContentHash(contents: string): string {
  return bytesToHex(sha256(new TextEncoder().encode(contents)));
}

export function readFileMetrics(element: HTMLElement): FileMetrics {
  const style = window.getComputedStyle(element);
  const pixels = (value: string, defaultValue: number) => {
    const text = value.trim();
    if (text === "0") return 0;
    if (!text.endsWith("px")) return defaultValue;
    const number = Number.parseFloat(text.slice(0, -2));
    return Number.isFinite(number) ? number : defaultValue;
  };
  return {
    hunkLineCount: 32,
    lineHeight: pixels(style.lineHeight, 21.6),
    diffHeaderHeight: 0,
    hunkSeparatorHeight: 32,
    spacing: pixels(style.getPropertyValue("--diffs-gap-block"), 0),
  };
}

export function equalFileMetrics(left: FileMetrics, right: FileMetrics): boolean {
  return left.hunkLineCount === right.hunkLineCount
    && left.lineHeight === right.lineHeight
    && left.diffHeaderHeight === right.diffHeaderHeight
    && left.hunkSeparatorHeight === right.hunkSeparatorHeight
    && left.spacing === right.spacing
    && left.paddingTop === right.paddingTop
    && left.paddingBottom === right.paddingBottom;
}

// Keep the Files language policy fixed when upstream adds new filename mappings.
// The original renderer matched the full filename, then compound/simple extensions.
const newerExtensions = new Set(["odin", "BUCK", "BUILD", "PACKAGE", "WORKSPACE", "Tiltfile", "bazel", "bxl", "bzl", "bzlmod", "star", "sky"]);
export function fileLanguage(filename: string): NonNullable<FileContents["lang"]> {
  const candidates = [filename, filename.match(/\.([^/\\]+\.[^/\\]+)$/)?.[1], filename.match(/\.([^.]+)$/)?.[1]];
  for (const candidate of candidates) {
    if (candidate !== undefined && !newerExtensions.has(candidate) && Object.hasOwn(EXTENSION_TO_FILE_FORMAT, candidate)) {
      return EXTENSION_TO_FILE_FORMAT[candidate as keyof typeof EXTENSION_TO_FILE_FORMAT]!;
    }
  }
  return "text";
}
