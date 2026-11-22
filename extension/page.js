// Chrome serializes this entire function for isolated-world execution in frame 0.
// Keep its dependencies inside the function and never evaluate command text.
export function pageAction(expectedUrl, command, args) {
  const fail = (error) => ({ error });
  const ok = (value) => ({ value });
  try {
    if (command === 'clickReceipt') {
      // Only read our isolated-world record. A hash change can preserve this
      // document, whereas a new document has no record for this click.
      const receipt = globalThis.hrafnClickReceipts?.get(args.id);
      receipt?.cleanup();
      return ok(receipt?.seen ?? false);
    }
    const expected = new URL(expectedUrl);
    if (!['http:', 'https:'].includes(expected.protocol) || location.href !== expected.href) {
      return fail('the web page changed before Hrafn could act');
    }

    if (command === 'watchClick') {
      const hit = document.elementFromPoint(args.x, args.y);
      if (!hit) return ok(false);
      const receipts = globalThis.hrafnClickReceipts ??= new Map();
      const record = { seen: false, cleanup };
      const timer = setTimeout(cleanup, 5000);
      receipts.set(args.id, record);
      window.addEventListener('click', observe, true);
      return ok(true);

      function observe(event) {
        if (event.isTrusted && event.button === 0 && event.composedPath().includes(hit) &&
            Math.abs(event.clientX - args.x) <= 2 && Math.abs(event.clientY - args.y) <= 2) {
          record.seen = true;
          window.removeEventListener('click', observe, true);
        }
      }

      function cleanup() {
        clearTimeout(timer);
        window.removeEventListener('click', observe, true);
        receipts.delete(args.id);
      }
    }

    if (command === 'settle') {
      return new Promise((resolve) => {
        let changedAt = performance.now();
        const deadline = changedAt + args.timeoutMs;
        const observer = new MutationObserver(() => { changedAt = performance.now(); });
        observer.observe(document, { childList: true, subtree: true, attributes: true, characterData: true });
        function check() {
          const now = performance.now();
          if (now >= deadline) {
            observer.disconnect();
            resolve(fail('main page did not settle before the timeout'));
          } else if (document.readyState === 'complete' && now - changedAt >= args.quietMs) {
            observer.disconnect();
            resolve(ok(true));
          } else {
            setTimeout(check, Math.min(50, deadline - now));
          }
        }
        check();
      });
    }

    if (command === 'probe') {
      return ok(args.selector != null
        ? document.querySelector(args.selector) !== null
        : (document.body?.innerText ?? '').includes(args.text));
    }

    const selector = args.target ?? args.ref;
    if (selector && /^e\d+$/.test(selector)) {
      return fail('accessibility refs need the debugger here; use a CSS selector');
    }
    const element = selector ? document.querySelector(selector) : document.body;
    if (command === 'read' && !selector && !element) return ok('');
    if (!element) return fail(`nothing matches \`${selector}\``);

    if (command === 'read') return ok(element.innerText ?? element.textContent ?? '');
    if (command === 'html') return ok(element.outerHTML);

    if (!element.getClientRects().length || getComputedStyle(element).visibility !== 'visible' || element.matches(':disabled')) {
      return fail('element is hidden or disabled');
    }

    switch (command) {
      case 'click':
        if (typeof element.click !== 'function') return fail('element cannot be clicked');
        element.click();
        return ok(null);
      case 'focus':
        if (typeof element.focus !== 'function') return fail('element cannot be focused');
        element.focus();
        return document.activeElement === element ? ok(null) : fail('element did not receive focus');
      case 'select':
        if (!(element instanceof HTMLSelectElement)) return fail('target is not a select element');
        if (![...element.options].some((option) => option.value === args.value && !option.matches(':disabled'))) {
          return fail('no enabled option has that value');
        }
        element.value = args.value;
        element.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return ok(element.value);
      case 'type': {
        const textInput = element instanceof HTMLInputElement &&
          ['text', 'search', 'password', 'email', 'tel', 'url', 'number'].includes(element.type);
        if (textInput || element instanceof HTMLTextAreaElement) {
          if (element.readOnly) return fail('element is read-only');
          element.focus();
          element.value = (args.append ? element.value : '') + args.text;
        } else if (element.isContentEditable) {
          element.focus();
          element.textContent = (args.append ? element.textContent : '') + args.text;
        } else {
          return fail('target is not an editable text field');
        }
        element.dispatchEvent(new InputEvent('input', { bubbles: true, composed: true, inputType: 'insertText', data: args.text }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return ok(null);
      }
      default:
        return fail('this action needs the debugger');
    }
  } catch (error) {
    return fail(error.message);
  }
}
