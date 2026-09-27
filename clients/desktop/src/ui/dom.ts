// The small pieces the page is built out of.
//
// The window draws almost everything from what the server sent rather than from
// markup, so these two exist to keep that from becoming a wall of
// `document.createElement` with nothing to read.

/** Finds an element the page is expected to have, and says so when it does not. */
export function element<T extends HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (!found) {
    throw new Error(`the page is missing #${id}`);
  }
  return found as T;
}

/** Builds an element with an optional class and text. */
export function make<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className !== undefined) {
    node.className = className;
  }
  if (text !== undefined) {
    node.textContent = text;
  }
  return node;
}

/** Builds a button that does one thing. */
export function button(
  label: string,
  onClick: () => void,
  options: { className?: string; title?: string; disabled?: boolean } = {},
): HTMLButtonElement {
  const node = make('button', options.className, label);
  node.type = 'button';
  if (options.title !== undefined) {
    node.title = options.title;
  }
  node.disabled = options.disabled ?? false;
  node.addEventListener('click', onClick);
  return node;
}
