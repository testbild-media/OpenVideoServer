// Printing a sheet, or keeping it as a PDF.
//
// The DMX patch table (owner, 2026-10-21: "Gerne direkt als PDF oder HTML to
// Print"). The window builds the sheet — it holds the show and the fixture
// list — and this puts it in a window of its own that nobody sees, and either
// hands it to the system's print dialog or writes it out as a PDF and opens
// that.
//
// A window of its own rather than the client's: printing the client's page
// would print the client. And a locked-down one, because what it shows is a
// document made of words from a show: no script runs in it, it has no bridge,
// and it cannot go anywhere.

import { writeFile } from 'node:fs/promises';

import { BrowserWindow, dialog, shell } from 'electron';

import type { Answer } from '../shared/types.js';

/** What to do with a sheet. */
export type PrintHow = 'print' | 'pdf';

/**
 * Prints `html`, or saves it as a PDF the operator names and opens it.
 *
 * Answers the file written for a PDF, and null for a print or for a save the
 * operator thought better of.
 */
export async function printSheet(
  html: string,
  how: PrintHow,
  suggested: string,
  parent: BrowserWindow | null,
): Promise<Answer<string | null>> {
  const sheet = new BrowserWindow({
    show: false,
    webPreferences: {
      javascript: false,
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  sheet.webContents.on('will-navigate', (event) => event.preventDefault());
  sheet.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));

  try {
    await sheet.loadURL(`data:text/html;charset=utf-8,${encodeURIComponent(html)}`);

    if (how === 'print') {
      const printed = await new Promise<{ ok: boolean; reason: string }>((resolve) => {
        sheet.webContents.print({ silent: false, printBackground: true }, (ok, reason) =>
          resolve({ ok, reason }),
        );
      });
      // Cancelled is the operator's answer, not a fault.
      if (!printed.ok && printed.reason !== 'cancelled') {
        return failed('print_failed', `the sheet could not be printed: ${printed.reason}`);
      }
      return { ok: true, value: null };
    }

    const pdf = await sheet.webContents.printToPDF({
      printBackground: true,
      // The sheet's own `@page`: A4 and its margins.
      preferCSSPageSize: true,
    });
    const options: Electron.SaveDialogOptions = {
      defaultPath: `${suggested}.pdf`,
      filters: [{ name: 'PDF', extensions: ['pdf'] }],
    };
    const chosen = await (parent === null || parent.isDestroyed()
      ? dialog.showSaveDialog(options)
      : dialog.showSaveDialog(parent, options));
    if (chosen.canceled || chosen.filePath === undefined) {
      return { ok: true, value: null };
    }
    await writeFile(chosen.filePath, pdf);
    // Opened, because a PDF asked for "directly" is one to look at now; a
    // viewer that will not open it leaves the file written all the same.
    void shell.openPath(chosen.filePath);
    return { ok: true, value: chosen.filePath };
  } catch (error) {
    return failed('print_failed', `the sheet could not be made: ${String(error)}`);
  } finally {
    sheet.destroy();
  }
}

function failed(code: string, message: string): Answer<string | null> {
  return { ok: false, code, message, details: [] };
}
