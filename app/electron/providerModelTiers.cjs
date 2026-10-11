function modelServiceTiers(value, defaultValue, label) {
  const tiers = value === undefined ? [] : value;
  if (!Array.isArray(tiers)) throw new Error(`${label} service tiers must be an array.`);
  const seen = new Set();
  const result = tiers.map((tier) => {
    if (!tier || typeof tier !== "object" || Array.isArray(tier)
      || typeof tier.id !== "string" || !tier.id || tier.id.trim() !== tier.id
      || tier.id.length > 80 || /[\u0000-\u001f\u007f]/.test(tier.id)
      || tier.id === "default" || seen.has(tier.id)
      || typeof tier.name !== "string" || !tier.name.trim()
      || typeof tier.description !== "string") {
      throw new Error(`${label} returned an invalid or repeated service tier.`);
    }
    seen.add(tier.id);
    return { id: tier.id, name: tier.name, description: tier.description };
  });
  if (defaultValue != null && (typeof defaultValue !== "string" || !seen.has(defaultValue))) {
    throw new Error(`${label} default service tier is not advertised.`);
  }
  return { serviceTiers: result, defaultServiceTier: defaultValue ?? null };
}

function validateModelServiceTier(model, value) {
  if (value === undefined || value === "") return;
  if (typeof value !== "string" || value.trim() !== value || value.length > 80
    || /[\u0000-\u001f\u007f]/.test(value)
    || (value !== "default" && !model.serviceTiers?.some((tier) => tier.id === value))) {
    throw new Error(`Model '${model.model}' does not advertise service tier '${String(value)}'. Choose an available speed.`);
  }
}

module.exports = { modelServiceTiers, validateModelServiceTier };
