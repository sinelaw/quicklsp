import * as assert from 'assert';
import * as vscode from 'vscode';

const EXTENSION_ID = 'sinelaw.quicklsp-vscode';

const EXPECTED_LANGUAGES: readonly string[] = [
  'c',
  'cpp',
  'rust',
  'go',
  'python',
  'javascript',
  'javascriptreact',
  'typescript',
  'typescriptreact',
  'java',
  'ruby',
];

suite('QuickLSP VS Code Extension', () => {
  suiteSetup(async function () {
    this.timeout(60_000);
    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext, `Extension '${EXTENSION_ID}' is not installed`);
    // Activating will try to spawn the quicklsp binary, which may not exist in
    // the test environment. The extension handles that by surfacing an error
    // message and leaving the client unstarted — activation itself still
    // succeeds and commands are registered.
    await ext.activate();
  });

  test('extension is registered and active', () => {
    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext, 'extension missing');
    assert.strictEqual(ext!.isActive, true, 'extension did not activate');
  });

  test('commands are registered', async () => {
    const commands = await vscode.commands.getCommands(true);
    assert.ok(
      commands.includes('quicklsp.restart'),
      "'quicklsp.restart' command is not registered"
    );
    assert.ok(
      commands.includes('quicklsp.showOutput'),
      "'quicklsp.showOutput' command is not registered"
    );
  });

  test('package.json declares activation for every supported language', () => {
    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext);
    const activationEvents = (ext!.packageJSON.activationEvents ?? []) as string[];
    for (const lang of EXPECTED_LANGUAGES) {
      assert.ok(
        activationEvents.includes(`onLanguage:${lang}`),
        `missing activation event for '${lang}'`
      );
    }
  });

  test('default quicklsp.languages setting covers every supported language', () => {
    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext);
    const props = ext!.packageJSON.contributes?.configuration?.properties ?? {};
    const defaults = props['quicklsp.languages']?.default as string[] | undefined;
    assert.ok(defaults, 'quicklsp.languages has no default');
    for (const lang of EXPECTED_LANGUAGES) {
      assert.ok(
        defaults!.includes(lang),
        `default quicklsp.languages is missing '${lang}'`
      );
    }
  });

  test('quicklsp.serverPath defaults to empty (auto-detect)', () => {
    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext);
    const props = ext!.packageJSON.contributes?.configuration?.properties ?? {};
    const serverPathSchema = props['quicklsp.serverPath'];
    assert.ok(serverPathSchema, 'quicklsp.serverPath schema missing');
    assert.strictEqual(
      serverPathSchema.default,
      '',
      'expected empty-string default so resolver can auto-detect bundled/dev/PATH'
    );
  });
});
