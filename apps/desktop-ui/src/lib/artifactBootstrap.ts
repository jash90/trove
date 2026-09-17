/**
 * The artifact sandbox, as one self-contained document.
 *
 * It is loaded into a sandboxed iframe (`allow-scripts`, never
 * `allow-same-origin`) as srcdoc, which matters for how it is written:
 * an inline classic script crosses no origin, needs no module resolution,
 * and asks no server for anything — the same document therefore runs in a
 * dev server and in the built application without a difference. The code
 * to show arrives from the parent as a message; HTML and SVG render into
 * a nested sandboxed frame (the sandbox is inherited), and JSX arrives
 * already compiled by the parent's on-demand Babel chunk, to run against
 * the minimal React this document carries.
 *
 * The minimal React is exactly that: createElement, fragments, useState,
 * useEffect, and a renderer that rebuilds the tree when state changes. It
 * is enough for the interface demos an artifact is; a component needing
 * more is told so, in so many words.
 */
export const ARTIFACT_BOOTSTRAP = `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  html, body { margin: 0; height: 100%; }
  body { font: 14px/1.5 -apple-system, "Segoe UI", sans-serif; color: #1c1c1e; }
  #root { min-height: 100%; }
  .artifact-error {
    margin: 12px;
    padding: 10px 12px;
    border: 1px solid #e2b5b5;
    border-radius: 8px;
    background: #fdf3f3;
    color: #9d3b3b;
    font: 12px/1.5 ui-monospace, monospace;
    white-space: pre-wrap;
  }
</style>
</head>
<body>
<div id="root"></div>
<script>
(function () {
  'use strict';

  // ------------------------------------------------------ minimal React --

  var Fragment = { __fragment: true };
  var currentHooks = null;
  var hookIndex = 0;
  var renderScheduled = false;
  var rootComponent = null;
  // Hooks live by position in the tree, not on the vdom nodes: a re-render
  // calls the components again and builds fresh nodes, and state that
  // moved with the nodes would reset on every keystroke. A component keeps
  // its hooks for as long as it keeps its place.
  var hooksByPath = {};

  function createElement(type, props) {
    var children = [];
    for (var i = 2; i < arguments.length; i++) children.push(arguments[i]);
    var allProps = props || {};
    var flat = [];
    function flatten(list) {
      list.forEach(function (item) {
        if (Array.isArray(item)) flatten(item);
        else if (item !== null && item !== undefined && item !== false) flat.push(item);
      });
    }
    flatten(children);
    allProps.children = flat;
    return { type: type, props: allProps };
  }

  function useState(initial) {
    if (!currentHooks) {
      throw new Error('useState can only be called while the component renders');
    }
    var slot = hookIndex;
    hookIndex += 1;
    var hooks = currentHooks;
    if (!(slot in hooks)) {
      hooks[slot] = [
        typeof initial === 'function' ? initial() : initial,
        function setValue(next) {
          var value = typeof next === 'function' ? next(hooks[slot][0]) : next;
          if (value !== hooks[slot][0]) {
            hooks[slot][0] = value;
            scheduleRender();
          }
        },
      ];
    }
    return hooks[slot];
  }

  function useEffect(effect, deps) {
    if (!currentHooks) {
      throw new Error('useEffect can only be called while the component renders');
    }
    var slot = hookIndex;
    hookIndex += 1;
    var hooks = currentHooks;
    if (!(slot in hooks)) hooks[slot] = { cleanup: null, deps: null, ran: false };
    var entry = hooks[slot];
    var changed =
      !entry.ran ||
      deps === undefined ||
      deps === null ||
      deps.length !== (entry.deps ? entry.deps.length : -1) ||
      (deps || []).some(function (dep, index) {
        return entry.deps && dep !== entry.deps[index];
      });
    if (changed) {
      if (typeof entry.cleanup === 'function') entry.cleanup();
      entry.deps = deps || [];
      entry.ran = true;
      pendingEffects.push(function () {
        entry.cleanup = effect() || null;
      });
    } else {
      entry.deps = deps || [];
    }
  }

  var pendingEffects = [];

  function buildDom(node, path) {
    if (typeof node === 'string' || typeof node === 'number') {
      return document.createTextNode(String(node));
    }
    if (!node || typeof node !== 'object') return document.createTextNode('');
    if (node.type === Fragment) {
      var fragment = document.createDocumentFragment();
      node.props.children.forEach(function (child, index) {
        fragment.appendChild(buildDom(child, path + '/f' + index));
      });
      return fragment;
    }
    if (typeof node.type === 'function') {
      var componentHooks = hooksByPath[path] || (hooksByPath[path] = {});
      var previousHooks = currentHooks;
      var previousIndex = hookIndex;
      currentHooks = componentHooks;
      hookIndex = 0;
      var rendered;
      try {
        rendered = node.type(node.props);
      } finally {
        currentHooks = previousHooks;
        hookIndex = previousIndex;
      }
      return buildDom(rendered, path);
    }
    var element = document.createElement(node.type);
    Object.keys(node.props || {}).forEach(function (key) {
      if (key === 'children') return;
      var value = node.props[key];
      if (key === 'className') {
        element.setAttribute('class', value);
      } else if (key === 'style' && value && typeof value === 'object') {
        Object.keys(value).forEach(function (property) {
          var css = value[property];
          var kebab = property.replace(/([A-Z])/g, '-$1').toLowerCase();
          element.style.setProperty(kebab, typeof css === 'number' ? String(css) : css);
        });
      } else if (key.slice(0, 2) === 'on' && typeof value === 'function') {
        element.addEventListener(key.slice(2).toLowerCase(), value);
      } else if (value === true) {
        element.setAttribute(key, '');
      } else if (value !== false && value !== null && value !== undefined) {
        element.setAttribute(key, value);
      }
    });
    node.props.children.forEach(function (child, index) {
      element.appendChild(buildDom(child, path + '/' + index));
    });
    return element;
  }

  function render(component) {
    rootComponent = component;
    renderScheduled = false;
    var container = document.getElementById('root');
    container.innerHTML = '';
    pendingEffects = [];
    container.appendChild(buildDom(component));
    var effects = pendingEffects.slice();
    pendingEffects = [];
    effects.forEach(function (effect) { effect(); });
  }

  function scheduleRender() {
    if (renderScheduled || !rootComponent) return;
    renderScheduled = true;
    Promise.resolve().then(function () { render(rootComponent); });
  }

  var React = { createElement: createElement, Fragment: Fragment, useState: useState, useEffect: useEffect };

  // --------------------------------------------------------- rendering --

  function note(message) {
    var root = document.getElementById('root');
    root.innerHTML = '';
    var box = document.createElement('div');
    box.className = 'artifact-error';
    box.textContent = message;
    root.appendChild(box);
  }

  // The sandbox flag is inherited by nested frames, so an HTML document
  // renders inside one rather than replacing this page: document.write
  // would take the listener with it, and the second preview in a session
  // would arrive to a page that no longer listens.
  function renderInFrame(source) {
    var root = document.getElementById('root');
    root.innerHTML = '';
    var frame = document.createElement('iframe');
    frame.setAttribute('title', 'artifact document');
    frame.setAttribute('sandbox', 'allow-scripts');
    frame.style.cssText = 'border:0;width:100%;height:100vh';
    // Set as a property, verbatim: the document parses the string as-is,
    // and escaping here would corrupt the quotes of the document's own
    // attributes (xmlns first among them).
    frame.srcdoc = source;
    root.appendChild(frame);
  }

  function runComponent(compiled) {
    try {
      var module_ = { exports: {} };
      var require_ = function (name) {
        if (name === 'react') return React;
        throw new Error('Module "' + name + '" is not available in the sandbox; react is.');
      };
      var runner = new Function('React', 'exports', 'module', 'require',
        '"use strict";\\n' + compiled);
      runner(React, module_.exports, module_, require_);
      var exported = module_.exports.default || module_.exports.App;
      if (typeof exported !== 'function') {
        note('Nothing to render: export a default component or a top-level App.\\n\\n'
          + 'jsx\\nexport default function App() {\\n  return <h1>Hello</h1>;\\n}');
        return;
      }
      render(createElement(exported));
    } catch (error) {
      note('The component threw while rendering:\\n\\n' + error);
    }
  }

  window.addEventListener('message', function (event) {
    var data = event.data || {};
    if (data.kind !== 'artifact') return;
    hooksByPath = {};
    if (data.language === 'html') {
      renderInFrame(data.source);
    } else if (data.language === 'svg') {
      if (data.source.indexOf('<svg') === -1) {
        note('That block is not an svg document.');
        return;
      }
      renderInFrame('<!doctype html><style>html,body{margin:0;height:100%;display:grid;'
        + 'place-items:center;background:#fff}svg{max-width:100%;max-height:100vh}</style>'
        + data.source);
    } else if (data.language === 'jsx') {
      if (data.error) note(data.error);
      else runComponent(data.compiled);
    }
  });

  window.parent.postMessage({ kind: 'artifact-ready' }, '*');
})();
</script>
</body>
</html>`;
