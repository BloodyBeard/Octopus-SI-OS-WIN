const fs = require("node:fs/promises");
const path = require("node:path");

async function planClientMaintenanceEe(home, id, template) {
  const file = path.join(home, "state", "ee_instances", `${id}.json`);
  let before;
  try { before = await fs.readFile(file, "utf8"); }
  catch (error) { if (error.code !== "ENOENT") throw error; }
  if (before === undefined) return [{ file, beforeDigest: null, next: {
    ee_id: id, template_name: template, parent_ee_id: null,
    goal: "Maintain the operator's client", config: {},
  } }];
  const current = JSON.parse(before);
  if (current.ee_id !== id || current.template_name !== template
      || typeof current.goal !== "string" || !current.goal.trim()
      || current.parent_ee_id !== null) {
    throw new Error("Client maintenance EE has another owner or invalid placement");
  }
  return [];
}

module.exports = { planClientMaintenanceEe };
