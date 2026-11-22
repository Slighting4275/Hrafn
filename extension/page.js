// Chrome serializes this entire function for isolated-world execution in frame 0.
// Keep its dependencies inside the function and never evaluate command text.
export function pageAction(expectedUrl, command, args) {
  const fail = (error) => ({ error });
  const ok = (value) => ({ value });
  try {
    const expected = new URL(expectedUrl);
    if (!['http:', 'https:'].includes(expected.protocol) || location.href !== expected.href) {
      return fail('the web page changed before Hrafn could act');
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
        return ok(null);
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
          ['text', 'search', 'password', 'email', 'tel', 'url'].includes(element.type);
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
