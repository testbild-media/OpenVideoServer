// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What a GPU connector is used for.
 *
 * This is server configuration, not show data: it describes the machine, and it must never be inferred from what happens to be plugged in. The reference server has no integrated GPU, so the single-GPU case where a service display and media outputs share one card is normal, not exceptional.
 */
export type ConnectorRole = 'SERVICE_DISPLAY' | 'MEDIA_OUTPUT' | 'UNUSED';

/**
 * Assigns a role to a connector.
 */
export interface SetConnectorRoleRequest {
  /**
   * Connector name.
   */
  connector: string;
  /**
   * PCI address of the GPU.
   */
  gpu_pci_address: string;
  /**
   * The role to assign.
   *
   * Always explicit. A connector's role is never inferred from what happens to be plugged in.
   */
  role: ConnectorRole;
}
