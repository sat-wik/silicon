import * as vscode from 'vscode';
import { execFile } from 'child_process';
import * as path from 'path';

// SARIF types — only the subset silicon produces.
interface SarifLocation {
    physicalLocation?: {
        artifactLocation?: { uri?: string };
        region?: { startLine?: number; startColumn?: number };
    };
}
interface SarifResult {
    ruleId?: string;
    level?: string;
    kind?: string;
    message?: { text?: string };
    locations?: SarifLocation[];
}
interface SarifRun { results?: SarifResult[] }
interface SarifLog { runs?: SarifRun[] }

const DIAGNOSTIC_SOURCE = 'silicon';
const diagnosticCollection = vscode.languages.createDiagnosticCollection(DIAGNOSTIC_SOURCE);

export function activate(context: vscode.ExtensionContext): void {
    context.subscriptions.push(diagnosticCollection);

    // Command: Silicon: Check File
    context.subscriptions.push(
        vscode.commands.registerCommand('silicon.check', () => {
            const editor = vscode.window.activeTextEditor;
            if (editor) {
                checkFile(editor.document);
            }
        })
    );

    // Auto-check on open
    context.subscriptions.push(
        vscode.workspace.onDidOpenTextDocument(doc => {
            if (isSupported(doc)) checkFile(doc);
        })
    );

    // Auto-check on save (if enabled)
    context.subscriptions.push(
        vscode.workspace.onDidSaveTextDocument(doc => {
            if (!isSupported(doc)) return;
            const cfg = vscode.workspace.getConfiguration('silicon');
            if (cfg.get<boolean>('runOnSave', true)) {
                checkFile(doc);
            }
        })
    );

    // Clear diagnostics for closed files
    context.subscriptions.push(
        vscode.workspace.onDidCloseTextDocument(doc => {
            diagnosticCollection.delete(doc.uri);
        })
    );
}

export function deactivate(): void {
    diagnosticCollection.dispose();
}

function isSupported(doc: vscode.TextDocument): boolean {
    return doc.languageId === 'c' || doc.languageId === 'cpp';
}

function checkFile(doc: vscode.TextDocument): void {
    const cfg = vscode.workspace.getConfiguration('silicon');
    const exe = cfg.get<string>('executablePath', 'silicon');
    const extraArgs = cfg.get<string[]>('extraArgs', []);

    // Prefer the workspace folder root; fall back to the file's directory.
    const wsFolder = vscode.workspace.getWorkspaceFolder(doc.uri)?.uri.fsPath
        ?? path.dirname(doc.uri.fsPath);

    const args = ['--format', 'sarif', ...extraArgs, doc.uri.fsPath];

    execFile(exe, args, { cwd: wsFolder }, (err, stdout, stderr) => {
        // silicon exits non-zero when findings exceed the threshold — that's
        // expected. Only bail if we couldn't parse the output at all.
        if (!stdout && err) {
            // Binary not found or crashed without SARIF output.
            vscode.window.showWarningMessage(
                `Silicon: could not run '${exe}': ${stderr || String(err)}`
            );
            return;
        }

        let sarif: SarifLog;
        try {
            sarif = JSON.parse(stdout) as SarifLog;
        } catch {
            return;
        }

        const diagsByUri = new Map<string, vscode.Diagnostic[]>();
        for (const run of sarif.runs ?? []) {
            for (const result of run.results ?? []) {
                const diag = resultToDiagnostic(result);
                if (!diag) continue;
                const uri = resultUri(result) ?? doc.uri.fsPath;
                if (!diagsByUri.has(uri)) diagsByUri.set(uri, []);
                diagsByUri.get(uri)!.push(diag);
            }
        }

        // Replace diagnostics for the checked file; preserve others.
        diagnosticCollection.delete(doc.uri);
        for (const [uriStr, diags] of diagsByUri) {
            const fileUri = uriStr.startsWith('/')
                ? vscode.Uri.file(uriStr)
                : vscode.Uri.file(path.resolve(wsFolder, uriStr));
            diagnosticCollection.set(fileUri, diags);
        }
    });
}

function resultToDiagnostic(result: SarifResult): vscode.Diagnostic | null {
    // Notes are informational — don't raise them as VS Code diagnostics.
    if (result.kind === 'informational' || result.level === 'note') return null;

    const loc = result.locations?.[0];
    const lineNo = (loc?.physicalLocation?.region?.startLine ?? 1) - 1;
    const range = new vscode.Range(lineNo, 0, lineNo, Number.MAX_SAFE_INTEGER);
    const message = result.message?.text ?? result.ruleId ?? 'silicon finding';

    const severity = result.level === 'error'
        ? vscode.DiagnosticSeverity.Error
        : vscode.DiagnosticSeverity.Warning;

    const diag = new vscode.Diagnostic(range, message, severity);
    diag.source = DIAGNOSTIC_SOURCE;
    if (result.ruleId) diag.code = result.ruleId;
    return diag;
}

function resultUri(result: SarifResult): string | undefined {
    return result.locations?.[0]?.physicalLocation?.artifactLocation?.uri;
}
