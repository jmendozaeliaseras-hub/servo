(function() {
  'use strict';

  // If already in reader mode, restore original page.
  if (document.body.getAttribute('data-reader-mode') === 'true') {
    location.reload();
    return;
  }

  // Heuristic: find the best content node.
  function scoreNode(node) {
    var score = 0;
    var text = node.innerText || '';
    score += Math.min(text.length / 100, 20);
    score += node.querySelectorAll('p').length * 3;
    var tag = node.tagName.toLowerCase();
    if (tag === 'nav' || tag === 'aside' || tag === 'footer' || tag === 'header') {
      score -= 50;
    }
    var ci = ((node.className || '') + ' ' + (node.id || '')).toLowerCase();
    if (/comment|sidebar|widget|footer|nav|menu|ad|social|share|related/.test(ci)) {
      score -= 30;
    }
    if (/article|content|post|entry|body|main|text|story/.test(ci)) {
      score += 25;
    }
    return score;
  }

  var content = document.querySelector('article') || document.querySelector('[role="main"]') || document.querySelector('main');

  if (!content) {
    var candidates = document.querySelectorAll('div, section, td');
    var bestScore = -Infinity;
    for (var i = 0; i < candidates.length; i++) {
      var s = scoreNode(candidates[i]);
      if (s > bestScore) {
        bestScore = s;
        content = candidates[i];
      }
    }
  }

  if (!content) {
    content = document.body;
  }

  // Extract title.
  var title = document.title || '';
  var h1 = document.querySelector('h1');
  if (h1 && h1.innerText) {
    title = h1.innerText;
  }

  // Clone and clean content using safe DOM manipulation.
  var clone = content.cloneNode(true);
  var removeTags = ['script', 'style', 'iframe', 'nav', 'aside', 'footer', 'header',
                    'form', 'button', 'input', 'select', 'textarea', 'noscript'];
  for (var t = 0; t < removeTags.length; t++) {
    var els = clone.querySelectorAll(removeTags[t]);
    for (var j = els.length - 1; j >= 0; j--) {
      els[j].parentNode.removeChild(els[j]);
    }
  }

  // Remove elements with ad/nav-like classes.
  var allEls = clone.querySelectorAll('*');
  for (var k = 0; k < allEls.length; k++) {
    var el = allEls[k];
    var classId = ((el.className || '') + ' ' + (el.id || '')).toLowerCase();
    if (/ad-|ads-|advert|sidebar|widget|share|social|comment|related|popup|modal|overlay|cookie|banner/.test(classId)) {
      if (el.parentNode) {
        el.parentNode.removeChild(el);
      }
    }
  }

  // Build reader view using safe DOM methods (no innerHTML with untrusted content).
  // The content in `clone` comes from the same page origin, so this is a same-origin
  // DOM restructuring, not injection of external content.

  // Clear existing styles.
  while (document.head.firstChild) {
    document.head.removeChild(document.head.firstChild);
  }

  var meta1 = document.createElement('meta');
  meta1.setAttribute('charset', 'UTF-8');
  document.head.appendChild(meta1);

  var meta2 = document.createElement('meta');
  meta2.setAttribute('name', 'viewport');
  meta2.setAttribute('content', 'width=device-width, initial-scale=1.0');
  document.head.appendChild(meta2);

  document.title = 'Reader: ' + title;

  var style = document.createElement('style');
  style.textContent =
    'body { font-family: Georgia, "Times New Roman", serif; line-height: 1.8; max-width: 680px; margin: 40px auto; padding: 0 20px; color: #333; background: #fafafa; }' +
    '@media (prefers-color-scheme: dark) { body { color: #d4d4d4; background: #1a1a1a; } a { color: #6db3f2; } }' +
    'h1 { font-size: 1.8em; line-height: 1.3; margin-bottom: 0.5em; }' +
    'img { max-width: 100%; height: auto; }' +
    'pre, code { font-size: 0.9em; overflow-x: auto; }' +
    'blockquote { border-left: 3px solid #ccc; margin-left: 0; padding-left: 1em; color: #666; }' +
    '@media (prefers-color-scheme: dark) { blockquote { border-left-color: #555; color: #aaa; } }';
  document.head.appendChild(style);

  // Clear body and rebuild with clean content.
  while (document.body.firstChild) {
    document.body.removeChild(document.body.firstChild);
  }

  var heading = document.createElement('h1');
  heading.textContent = title;
  document.body.appendChild(heading);

  // Append the cleaned content nodes (same-origin DOM nodes, not external HTML).
  while (clone.firstChild) {
    document.body.appendChild(clone.firstChild);
  }

  document.body.setAttribute('data-reader-mode', 'true');
  window.scrollTo(0, 0);
})();
