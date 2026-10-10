export type OperatorPermissionChoice = {
  decision: "accept" | "decline";
  persistence: "once" | "session" | "always";
};

export type OperatorPermissionOffer = {
  client_interface: string;
  offered_choices: OperatorPermissionChoice[];
};

export type OperatorPermissionAnswer = {
  decision: "accept" | "decline" | "cancel";
  selected_persistence?: OperatorPermissionChoice["persistence"];
};

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The permission request is incomplete.");
  return value as Record<string, unknown>;
}

export function operatorPermissionOffer(data: Record<string, unknown>, clientInterface?: string): OperatorPermissionOffer | null {
  const detail = record(data.detail);
  if (Object.hasOwn(detail, "consent_offer")) throw new Error("This request uses a retired consent contract. A fresh request is required.");
  if (!Object.hasOwn(detail, "permission_offer")) {
    if (data.kind === "browser_history_read") throw new Error("The history request has no permission choices.");
    return null;
  }
  const offer = record(detail.permission_offer);
  const client = offer.client_interface;
  if (Object.keys(offer).some((key) => !["client_interface", "offered_choices"].includes(key))
    || typeof client !== "string" || !client || client.trim() !== client || client.includes("\0")
    || new TextEncoder().encode(client).length > 4096 || clientInterface !== undefined && client !== clientInterface
    || !Array.isArray(offer.offered_choices) || offer.offered_choices.length < 1 || offer.offered_choices.length > 6) {
    throw new Error("The permission choices have no exact client identity.");
  }
  const pairs = new Set<string>();
  const choices = offer.offered_choices.map((raw): OperatorPermissionChoice => {
    const choice = record(raw);
    if (Object.keys(choice).length !== 2 || Object.keys(choice).some((key) => !["decision", "persistence"].includes(key))
      || choice.decision !== "accept" && choice.decision !== "decline"
      || choice.persistence !== "once" && choice.persistence !== "session" && choice.persistence !== "always"
      || data.kind === "browser_history_read" && choice.persistence === "session") {
      throw new Error("The permission request contains an unsupported choice.");
    }
    const pair = `${choice.decision}:${choice.persistence}`;
    if (pairs.has(pair)) throw new Error("The permission request contains duplicate choices.");
    pairs.add(pair);
    return { decision: choice.decision, persistence: choice.persistence };
  });
  return { client_interface: client, offered_choices: choices };
}

export function operatorPermissionAnswer(data: Record<string, unknown>, value: unknown, clientInterface: string): OperatorPermissionAnswer {
  const offer = operatorPermissionOffer(data, clientInterface);
  const answer: Record<string, unknown> = typeof value === "string" ? { decision: value } : record(value);
  if (Object.keys(answer).some((key) => !["decision", "selected_persistence"].includes(key))
    || answer.decision !== "accept" && answer.decision !== "decline" && answer.decision !== "cancel") {
    throw new Error("The tool approval has no supported decision.");
  }
  const selected = answer.selected_persistence;
  if (!offer || answer.decision === "cancel") {
    if (Object.hasOwn(answer, "selected_persistence")) throw new Error("This response cannot select a persistence scope.");
    return { decision: answer.decision };
  }
  if (selected !== "once" && selected !== "session" && selected !== "always"
    || !offer.offered_choices.some((choice) => choice.decision === answer.decision && choice.persistence === selected)) {
    throw new Error("The response is not one of the exact offered choices.");
  }
  return { decision: answer.decision, selected_persistence: selected };
}
