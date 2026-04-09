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
  const configuredPath = config.get<string>('serverPath', 'quicklsp');
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

  const resolvedPath = resolveServerPath(configuredPath);
  if (!resolvedPath) {
    const message =
      `QuickLSP: could not find the 'quicklsp' executable at '${configuredPath}'. ` +
      `Install it (e.g. 'cargo install --path .') or set 'quicklsp.serverPath' in your settings.`;
    outputChannel?.appendLine(message);
    vscode.window.showErrorMessage(message);
    return;
  }

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

/**
 * Resolve the configured server path. If the path is absolute or relative to a
 * workspace folder we check that it exists; otherwise we defer to the OS to
 * locate it via PATH by returning the command name as-is.
 */
function resolveServerPath(configured: string): string | undefined {
  if (!configured) {
    return undefined;
  }

  if (path.isAbsolute(configured)) {
    return fs.existsSync(configured) ? configured : undefined;
  }

  // Relative paths are resolved against the first workspace folder, if any.
  const folders = vscode.workspace.workspaceFolders;
  if (folders && folders.length > 0 && (configured.includes('/') || configured.includes('\\'))) {
    const candidate = path.resolve(folders[0].uri.fsPath, configured);
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }

  // Bare command name — assume it's on PATH and let the OS resolve it.
  return configured;
}
