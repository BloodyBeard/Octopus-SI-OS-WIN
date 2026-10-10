function parseScheduledDueAt(value) {
  const match = typeof value === "string" && /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})$/.exec(value);
  if (!match || match[8] === "-00:00") throw new Error("schedule.due_at requires an absolute RFC3339 timestamp with a known UTC offset");
  const [year, month, day, hour, minute, second] = match.slice(1, 7).map(Number);
  const date = new Date(0);
  date.setUTCFullYear(year, month - 1, day);
  date.setUTCHours(hour, minute, second, 0);
  if (date.getUTCFullYear() !== year || date.getUTCMonth() !== month - 1 || date.getUTCDate() !== day
    || date.getUTCHours() !== hour || date.getUTCMinutes() !== minute || date.getUTCSeconds() !== second || year < 1) {
    throw new Error("schedule.due_at is not a valid calendar timestamp");
  }
  let offset = 0;
  if (match[8] !== "Z") {
    const hours = Number(match[8].slice(1, 3));
    const minutes = Number(match[8].slice(4, 6));
    if (hours > 23 || minutes > 59) throw new Error("schedule.due_at has an invalid UTC offset");
    offset = (hours * 60 + minutes) * 60_000 * (match[8][0] === "+" ? 1 : -1);
  }
  const nanoseconds = BigInt(date.getTime() - offset) * 1_000_000n + BigInt((match[7] || "0").padEnd(9, "0"));
  if (nanoseconds < 0n || nanoseconds > 9_223_372_036_854_775_807n) throw new Error("schedule.due_at is outside the supported timestamp range");
  return nanoseconds;
}

module.exports = { parseScheduledDueAt };
