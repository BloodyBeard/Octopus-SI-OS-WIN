export const PLAY_SM_PATH = "M8 16.7229V7.27711C8 6.29075 9.08894 5.69298 9.9211 6.22254L17.3428 10.9454C18.1147 11.4366 18.1147 12.5634 17.3428 13.0546L9.92109 17.7775C9.08894 18.3071 8 17.7093 8 16.7229Z";

export function translatedAbsoluteCubicPath(path: string, dx: number, dy: number, significantDigits = 6): string {
  const commands = path.match(/[MLCVHZ][^MLCVHZ]*/g);
  if (!commands || commands.join("") !== path) throw new Error("Unsupported absolute SVG path");
  return commands.map(part => {
    const command = part[0];
    const args = part.slice(1).trim();
    const values = args ? args.split(/[ ,]+/).map(Number) : [];
    const groupSize = command === "C" ? 6 : command === "M" || command === "L" ? 2 : command === "Z" ? 0 : 1;
    if (values.some(value => !Number.isFinite(value)) || (groupSize === 0 ? values.length !== 0 : !values.length || values.length % groupSize !== 0)) throw new Error("Unsupported absolute SVG coordinates");
    return command + values.map((value, index) => {
      const offset = command === "V" ? dy : command === "H" ? dx : index % 2 === 0 ? dx : dy;
      return String(Number((value + offset).toPrecision(significantDigits)));
    }).join(" ");
  }).join("");
}
