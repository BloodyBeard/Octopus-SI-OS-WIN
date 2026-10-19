const BUILTIN_INPUTS = new Set(["gesture.intent"]);

export function hasBuiltinClientExtensionInput(inputId: string): boolean {
  return BUILTIN_INPUTS.has(inputId);
}
