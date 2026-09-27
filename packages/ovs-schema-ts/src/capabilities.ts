// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What a GPU connector is used for.
 *
 * This is server configuration, not show data: it describes the machine, and it must never be inferred from what happens to be plugged in. The reference server has no integrated GPU, so the single-GPU case where a service display and media outputs share one card is normal, not exceptional.
 */
export type ConnectorRole = 'SERVICE_DISPLAY' | 'MEDIA_OUTPUT' | 'UNUSED';

/**
 * What this server can do.
 *
 * A client reads this before offering anything, so that a server without, say, a second GPU does not present options that cannot work.
 */
export interface CapabilitiesResponse {
  /**
   * Major version of this API.
   */
  api_version: number;
  /**
   * Whether an operator account exists yet.
   *
   * `false` means the server is in first-run setup.
   */
  configured: boolean;
  /**
   * Physical connectors the renderer can drive.
   *
   * Its own card's, and the only ones a show output may be put on: the renderer enumerates displays on the one Vulkan device it renders with and matches roles against the names that device reports.
   */
  connectors: ConnectorInfo[];
  /**
   * Sockets on the other cards in the machine, which the renderer cannot drive.
   *
   * The System screen is chosen by *card* — wlroots takes a card, not a connector — so these can carry it, and on a machine with two GPUs that is where it belongs: the show's card should not also be carrying a browser. They can never be a show output, which is why they are a second list rather than more of the first.
   *
   * Named by the kernel (`eDP-1`) rather than by Vulkan (`monitor`), because no renderer has seen them. The two layers disagree about names and there is no safe way to map one onto the other, so nothing tries.
   */
  connectors_elsewhere?: ConnectorInfo[];
  /**
   * Codecs this server has a decoder for, by `FFmpeg`'s short name.
   *
   * The server plays what it is given, so this is what a client consults before offering to convert a file rather than send it. It is the interesting subset rather than every decoder in the binary, and it is asked of the binary rather than declared in the source.
   *
   * Empty means *this server cannot say* — a build with no `FFmpeg` — and must not be read as "plays nothing".
   */
  media_codecs: string[];
  /**
   * Every mode an output published on the network may run.
   *
   * Fixed by this build rather than by the hardware: a stream has no panel to satisfy, and what a receiver in another room expects is a broadcast raster. Sent so that a client offers what this server actually allows instead of carrying its own copy of the list — two copies are two lists, and the one edited less often is the one that is wrong.
   *
   * Whole modes rather than a number of hertz, because the rates are ratios: 23.98 is 24000/1001, and a receiver locking to it written as a decimal drifts against everything else in the building.
   */
  network_output_modes: DisplayMode[];
  /**
   * Realtime protocol version the server speaks.
   */
  realtime_protocol_version: number;
  /**
   * Server build version.
   */
  server_version: string;
  /**
   * Show schema version the server writes.
   */
  show_schema_version: number;
}
/**
 * A GPU connector as discovered on the server.
 */
export interface ConnectorInfo {
  /**
   * Whether something is currently plugged in.
   */
  connected: boolean;
  /**
   * Connector name as the driver reports it, for example `DP-1`.
   */
  connector: string;
  /**
   * PCI address of the GPU driving it.
   */
  gpu_pci_address: string;
  /**
   * EDID model name, when the attached device reports one.
   *
   * For recognising hardware in the client and for support calls. It decides nothing: outputs are forced onto connectors.
   */
  model?: string | null;
  /**
   * The modes this connector offers, as the driver reports them.
   *
   * Empty until a renderer has said. An operator choosing a mode for a wall needs the list, and inventing one would offer modes the panel cannot do.
   */
  modes?: ConnectorModeInfo[];
  /**
   * What this connector is configured for.
   */
  role: ConnectorRole;
}
/**
 * One mode a connector offers.
 */
export interface ConnectorModeInfo {
  /**
   * Refresh rate in millihertz, as the driver reports it.
   *
   * Not a float: 59 940 is 60000/1001 exactly, and the two rates a panel actually has must not round into one.
   */
  refresh_millihertz: number;
  /**
   * Resolution in pixels.
   */
  size: SizePx;
}
/**
 * A size in whole pixels.
 */
export interface SizePx {
  /**
   * Height in pixels.
   */
  height: number;
  /**
   * Width in pixels.
   */
  width: number;
}
/**
 * A display mode: resolution plus exact refresh rate.
 */
export interface DisplayMode {
  /**
   * Refresh rate as an exact rational value.
   */
  refresh: FrameRate;
  /**
   * Resolution in pixels.
   */
  size: SizePx;
}
/**
 * An exact frame rate expressed as a rational number of frames per second.
 *
 * Rational rather than floating point so that 60000/1001 stays exact and the scheduler cannot accumulate drift.
 */
export interface FrameRate {
  /**
   * Denominator, for example `1001`.
   */
  denominator: number;
  /**
   * Numerator, for example `60000`.
   */
  numerator: number;
}
