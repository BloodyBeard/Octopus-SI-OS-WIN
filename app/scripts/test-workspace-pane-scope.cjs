const assert = require('node:assert/strict');
const { test } = require('node:test');
const fs = require('node:fs');
const path = require('node:path');
const postcss = require('postcss');
const { JSDOM } = require('jsdom');

test('expanding a workspace preserves the nested task and its independent pane layout', () => {
  const stylesheet = postcss.parse(fs.readFileSync(path.join(__dirname, '../src/shell.css'), 'utf8'));
  const rules = [];
  stylesheet.walkRules(rule => {
    if (rule.selector.startsWith('.workspace-pane')) rules.push(rule.toString());
  });
  const dom = new JSDOM(`<style>${rules.join('\n')}</style>
    <div id="outer" class="workspace-pane-manager">
      <section id="home" class="workspace-pane workspace-pane-primary"></section>
      <aside id="task" class="workspace-pane workspace-pane-secondary">
        <div class="workspace-pane-secondary-clip"><div class="workspace-pane-secondary-content">
          <div id="inner" class="workspace-pane-manager">
            <section id="composer" class="workspace-pane workspace-pane-primary"></section>
            <aside id="browser" class="workspace-pane workspace-pane-secondary">
              <div id="resize" class="workspace-pane-separator"></div>
            </aside>
          </div>
        </div></div>
      </aside>
    </div>`);
  const { document } = dom.window;
  const style = id => dom.window.getComputedStyle(document.getElementById(id));
  for (const state of ['is-secondary-focused', 'is-secondary-overlay', '']) {
    document.getElementById('outer').className = `workspace-pane-manager ${state}`;
    assert.equal(style('composer').opacity, '1');
    assert.notEqual(style('composer').visibility, 'hidden');
    assert.equal(style('browser').position, 'relative');
    assert.notEqual(style('resize').display, 'none');
    assert.equal(style('home').visibility === 'hidden', state === 'is-secondary-focused');
  }
  document.getElementById('inner').classList.add('is-secondary-focused');
  assert.equal(style('composer').visibility, 'hidden');
  assert.equal(style('resize').display, 'none');
  assert.notEqual(style('home').visibility, 'hidden');
  dom.window.close();
});
