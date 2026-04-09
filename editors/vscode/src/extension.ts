import * as path from 'path';
import * as fs from 'fs';
import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
  RevealOutputChannelOn,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;
let outputChannel: vscode.OutputChannel | undefined;

/**
 * All VS Code language IDs that QuickLSP can serve. These map to the file
 * extensions handled by the underlying tree-sitter parsers in quicklsp
 * (C, C++, Rust, Go, Python, JavaScript, TypeScript, Java, Ruby).
 */
const ALL_SUPPORTED_LANGUAGES: readonly string[] = [
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

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  outputChannel = vscode.window.createOutputChannel('QuickLSP');
  context.subscriptions.push(outputChannel);

  context.subscriptions.push(
    vscode.commands.registerCommand('quicklsp.restart', async () => {
      await restartClient(context);
    }),
    vscode.commands.registerCommand('quicklsp.showOutput', () => {
      outputChannel?.show(true);
    })
  );

  // Restart the client automatically if the user changes any quicklsp.* setting.
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration('quicklsp')) {
        outputChannel?.appendLine('Configuration changed — restarting QuickLSP.');
        await restartClient(context);
      }
    })
  );

  await startClient(context);
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
    client = undefined;
  }
}

async function startClient(context: vscode.ExtensionContext): Promise<void> {
  const config = vscode.workspace.getConfiguration('quicklsp');
  const serverArgs = config.get<string[]>('serverArgs', []);
  const logLevel = config.get<string>('logLevel', 'info');
  const configuredLanguages = config.get<string[]>('languages', [...ALL_SUPPORTED_LANGUAGES]);

  const selectedLanguages = configuredLanguages.filter((lang) =>
    ALL_SUPPORTED_LANGUAGES.includes(lang)
  );

  if (selectedLanguages.length === 0) {
    outputChannel?.appendLine(
      'No supported languages enabled in quicklsp.languages — not starting the server.'
    );
    return;
  }

  // Detect an explicit user override (as opposed to the default 'quicklsp'
  // value). Only an override blocks fallback to the bundled binary.
  const inspection = config.inspect<string>('serverPath');
  const explicitOverride =
    inspection?.workspaceFolderValue ??
    inspection?.workspaceValue ??
    inspection?.globalValue;

  const resolved = resolveServerPath(context, explicitOverride);
  if (!resolved) {
    const attempted = explicitOverride ?? '(auto-detect)';
    const message =
      `QuickLSP: could not locate the 'quicklsp' executable (tried: ${attempted}). ` +
      `Install a release .vsix bundled with the server, build 'cargo build --release' ` +
      `in the repo, or set 'quicklsp.serverPath' explicitly.`;
    outputChannel?.appendLine(message);
    vscode.window.showErrorMessage(message);
    return;
  }

  const { path: resolvedPath, source } = resolved;
  outputChannel?.appendLine(`Resolved quicklsp from: ${source}`);

  outputChannel?.appendLine(`Starting QuickLSP server: ${resolvedPath} ${serverArgs.join(' ')}`);
  outputChannel?.appendLine(`Enabled languages: ${selectedLanguages.join(', ')}`);

  const serverOptions: ServerOptions = {
    run: {
      command: resolvedPath,
      args: serverArgs,
      transport: TransportKind.stdio,
      options: {
        env: {
          ...process.env,
          RUST_LOG: process.env.RUST_LOG ?? logLevel,
        },
      },
    },
    debug: {
      command: resolvedPath,
      args: serverArgs,
      transport: TransportKind.stdio,
      options: {
        env: {
          ...process.env,
          RUST_LOG: process.env.RUST_LOG ?? logLevel,
        },
      },
    },
  };

  const documentSelector = selectedLanguages.flatMap((language) => [
    { scheme: 'file', language },
    { scheme: 'untitled', language },
  ]);

  const clientOptions: LanguageClientOptions = {
    documentSelector,
    outputChannel,
    revealOutputChannelOn: RevealOutputChannelOn.Never,
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher(
        '**/*.{c,h,cpp,cc,cxx,hpp,hxx,rs,go,py,pyi,js,jsx,mjs,cjs,ts,mts,tsx,java,rb}'
      ),
    },
  };

  client = new LanguageClient(
    'quicklsp',
    'QuickLSP',
    serverOptions,
    clientOptions
  );

  try {
    await client.start();
    outputChannel?.appendLine('QuickLSP server started.');
  } catch (err) {
    const message = `QuickLSP: failed to start server: ${err instanceof Error ? err.message : String(err)}`;
    outputChannel?.appendLine(message);
    vscode.window.showErrorMessage(message);
  }
}

async function restartClient(context: vscode.ExtensionContext): Promise<void> {
  if (client) {
    try {
      await client.stop();
    } catch (err) {
      outputChannel?.appendLine(
        `Error stopping QuickLSP: ${err instanceof Error ? err.message : String(err)}`
      );
    }
    client = undefined;
  }
  await startClient(context);
}

interface ResolvedServer {
  path: string;
  source: string;
}

/**
 * Locate the `quicklsp` binary.
 *
 * Priority:
 *   1. Explicit `quicklsp.serverPath` override from user/workspace settings.
 *   2. Binary bundled inside a platform-specific `.vsix`
 *      (`<extensionPath>/server/quicklsp[.exe]`).
 *   3. Repo-local release build used during extension development
 *      (`<extensionPath>/../../target/release/quicklsp[.exe]`).
 *   4. Bare command name `quicklsp` — deferred to the OS PATH resolver.
 */
function resolveServerPath(
  context: vscode.ExtensionContext,
  explicitOverride: string | undefined
): ResolvedServer | undefined {
  const exeName = process.platform === 'win32' ? 'quicklsp.exe' : 'quicklsp';

  // 1. Explicit override — trust it, but still verify existence for absolute
  //    or workspace-relative paths so users get a clear error.
  if (explicitOverride && explicitOverride.trim().length > 0) {
    const override = explicitOverride.trim();

    if (path.isAbsolute(override)) {
      return fs.existsSync(override)
        ? { path: override, source: `explicit (${override})` }
        : undefined;
    }

    if (override.includes('/') || override.includes('\\')) {
      const folders = vscode.workspace.workspaceFolders;
      if (folders && folders.length > 0) {
        const candidate = path.resolve(folders[0].uri.fsPath, override);
        if (fs.existsSync(candidate)) {
          return { path: candidate, source: `explicit relative (${candidate})` };
        }
      }
      return undefined;
    }

    // Bare command name — defer to PATH.
    return { path: override, source: `explicit on PATH (${override})` };
  }

  // 2. Bundled with a platform-specific .vsix.
  const bundled = path.join(context.extensionPath, 'server', exeName);
  if (fs.existsSync(bundled)) {
    return { path: bundled, source: `bundled (${bundled})` };
  }

  // 3. Repo-local dev build (useful when launching via F5 in this repo).
  const repoDev = path.resolve(
    context.extensionPath,
    '..',
    '..',
    'target',
    'release',
    exeName
  );
  if (fs.existsSync(repoDev)) {
    return { path: repoDev, source: `repo-local dev build (${repoDev})` };
  }

  // 4. Last resort: bare command name, let the OS search PATH.
  return { path: exeName, source: `PATH lookup (${exeName})` };
}
