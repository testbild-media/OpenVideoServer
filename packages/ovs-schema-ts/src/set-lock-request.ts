// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What a lock is put on: one effect, or one generator.
 */
export type LockTarget =
  | {
      /**
       * The instance.
       */
      effect_instance_id: EffectInstanceId;
      kind: 'effect';
    }
  | {
      /**
       * The instance.
       */
      generator_instance_id: GeneratorInstanceId;
      kind: 'generator';
    };
/**
 * EffectInstanceId (ULID, 26 characters)
 */
export type EffectInstanceId = string;
/**
 * GeneratorInstanceId (ULID, 26 characters)
 */
export type GeneratorInstanceId = string;

/**
 * Locks one effect or generator where it is, or unlocks it.
 *
 * **Administrators only, and the only way a lock changes** (owner, 2026-10-19). A locked effect or generator cannot be removed, moved or put in another order by anybody, an administrator included; unlocking is this request, made deliberately, before any of those. A show sent whole carries the locks it already has whatever its copy says, so an editor's draft from before a lock cannot undo one by being saved.
 *
 * What it is doing is not locked: its values follow the patch like any other module's, and switching an effect on or off stays everybody's.
 */
export interface SetLockRequest {
  /**
   * Locked, or not.
   */
  locked: boolean;
  /**
   * What to lock or unlock.
   */
  target: LockTarget;
}
