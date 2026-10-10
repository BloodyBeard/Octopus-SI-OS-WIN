type ModalLayers = {
  layers: HTMLElement[];
  isolated: Map<HTMLElement, boolean>;
  overflow: string;
};

const modalDocuments = new WeakMap<Document, ModalLayers>();

export function isolateModalLayer(layer: HTMLElement): () => void {
  const document = layer.ownerDocument;
  const state = modalDocuments.get(document) ?? {
    layers: [], isolated: new Map<HTMLElement, boolean>(), overflow: document.body.style.overflow,
  };
  state.layers.push(layer);
  modalDocuments.set(document, state);
  updateIsolation(document, state);
  document.body.style.overflow = "hidden";
  let released = false;
  return () => {
    if (released) return;
    released = true;
    state.layers.splice(state.layers.indexOf(layer), 1);
    updateIsolation(document, state);
    if (!state.layers.length) {
      document.body.style.overflow = state.overflow;
      modalDocuments.delete(document);
    }
  };
}

function updateIsolation(document: Document, state: ModalLayers) {
  const targets = new Set<HTMLElement>();
  const top = state.layers.filter((layer) => layer.isConnected)
    .sort((left, right) => left.compareDocumentPosition(right) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1).at(-1);
  if (top) {
    for (let branch: HTMLElement = top; branch.parentElement; branch = branch.parentElement) {
      const parent = branch.parentElement;
      for (const sibling of parent.children) {
        if (sibling instanceof HTMLElement && sibling !== branch) targets.add(sibling);
      }
      if (parent === document.body) break;
    }
  }
  for (const [element, inert] of state.isolated) {
    if (targets.has(element)) continue;
    element.toggleAttribute("inert", inert);
    state.isolated.delete(element);
  }
  for (const element of targets) {
    if (!state.isolated.has(element)) state.isolated.set(element, element.hasAttribute("inert"));
    element.setAttribute("inert", "");
  }
}

export function isTopmostModal(root: HTMLElement | null): boolean {
  if (!root) return false;
  const modals = [...root.ownerDocument.querySelectorAll<HTMLElement>('[role="dialog"][aria-modal="true"]')]
    .filter((element) => !element.closest('[inert], [hidden], [aria-hidden="true"], [data-state="closed"]'));
  return modals.at(-1) === root;
}

export function restoreModalFocus(element: HTMLElement | null): void {
  if (element?.isConnected && !element.closest('[inert], [hidden], [aria-hidden="true"]')) element.focus({ preventScroll: true });
}

export function containModalFocus(event: Pick<KeyboardEvent, "key" | "shiftKey" | "defaultPrevented" | "preventDefault">, root: HTMLElement | null): void {
  if (event.key !== "Tab" || event.defaultPrevented || !root || !isTopmostModal(root)) return;
  const focusable = [...root.querySelectorAll<HTMLElement>(
    "button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [contenteditable='true'], [tabindex]:not([tabindex='-1'])",
  )].filter((element) => !element.closest('[inert], [hidden], [aria-hidden="true"]') && element.getClientRects().length > 0);
  const active = root.ownerDocument.activeElement;
  const first = focusable[0];
  const last = focusable.at(-1);
  if (!first || !last) {
    event.preventDefault();
    root.focus();
  } else if (event.shiftKey && (active === first || active === root)) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && (active === last || active === root)) {
    event.preventDefault();
    first.focus();
  }
}
