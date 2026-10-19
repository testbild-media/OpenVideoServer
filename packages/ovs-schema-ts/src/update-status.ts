// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What this server knows about updating itself.
 *
 * The owner, 2026-10-19: "ob der server selber Updates über das Repo abfragen kann und bei bedarf durch den User in der AdminGUI direkt installiert werden kann" — and "gar nicht automatisch prüfen. nur auf knopfdruck." So nothing here is ever fetched unasked: `check` is empty until an administrator has pressed the button since the server started.
 */
export interface UpdateStatus {
  /**
   * What the last check found, if anybody has asked since the server started.
   */
  check?: UpdateCheck | null;
  /**
   * What the installer last did, if it has ever run on this machine.
   */
  install?: UpdateProgress | null;
  /**
   * The version this server is running.
   */
  installed: string;
}
/**
 * What asking the repository for its newest release found.
 */
export interface UpdateCheck {
  /**
   * When the repository was asked, in milliseconds since the epoch.
   */
  checked_at: number;
  /**
   * The newest release's version, such as `0.2.46`, when there was an answer.
   */
  latest?: string | null;
  /**
   * Whether that is newer than what is installed, and carries a package for this machine — which is the one case in which there is something to install.
   */
  newer: boolean;
  /**
   * The release's own page, where what changed is written.
   */
  notes_url?: string | null;
  /**
   * Why there is no answer, or why the newest release cannot be installed here: no connection, no package for this machine.
   */
  problem?: string | null;
  /**
   * When that release was published, as the repository writes it.
   */
  published_at?: string | null;
}
/**
 * Where the installer is, or where it stopped.
 *
 * Written by the installer itself, which runs apart from the server: the server is restarted by the package it installs, and an install it ran itself would be killed half way.
 */
export interface UpdateProgress {
  /**
   * When it last wrote this, as UTC in ISO 8601.
   */
  at: string;
  /**
   * What it is doing, or why it stopped, in a sentence.
   */
  detail: string;
  /**
   * `starting`, `downloading`, `verifying`, `installing`, then `installed`, `failed` or `refused`.
   */
  stage: string;
  /**
   * The version it was asked for.
   */
  version: string;
}
