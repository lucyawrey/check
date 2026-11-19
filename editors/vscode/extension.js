const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

function activate() {
  const command = vscode.workspace.getConfiguration("check").get("serverPath");
  client = new LanguageClient(
    "check",
    "Check",
    { command, args: ["lsp"] },
    { documentSelector: [{ scheme: "file", language: "check" }] },
  );
  return client.start();
}

function deactivate() {
  return client?.stop();
}

module.exports = { activate, deactivate };
