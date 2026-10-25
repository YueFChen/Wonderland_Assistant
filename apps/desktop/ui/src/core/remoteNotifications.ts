export type RemoteInteractionState = 'ready' | 'transferring' | 'delivered'

export interface RemoteInteraction {
  id: string
  rootPluginId: string
  pluginId: string
  kind: 'pick' | 'read' | 'download' | 'open_url' | 'notice'
  state: RemoteInteractionState
  payload: { name?: string; size?: number; extensions?: string[]; maxBytes?: number; handle?: string; offset?: number; length?: number; url?: string; message?: string }
}

export interface RemoteInteractionNotification {
  kind: 'interaction' | 'download'
  id: string
  rootPluginId: string
  pluginId: string
  state: 'ready' | 'replied' | 'transferring' | 'delivered' | 'dismissed' | 'expired' | 'cancelled'
}

export type RemoteNotification =
  | { kind: 'plugin_event'; payload: unknown }
  | { kind: 'plugin_state' }
  | { kind: 'interaction_change'; payload: RemoteInteractionNotification }

export interface RemoteNotificationBatch {
  cursor: number
  events: RemoteNotification[]
  missed: boolean
}

/** A missed cursor invalidates every reconstructible snapshot before applying new events. */
export function dispatchRemoteNotifications(batch: RemoteNotificationBatch, emit: (name: string, payload: unknown) => void) {
  if (batch.missed) {
    emit('plugins:changed', null)
    emit('interactions:changed', null)
  }
  for (const event of batch.events) {
    switch (event.kind) {
      case 'plugin_event': emit('plugin:event', event.payload); break
      case 'plugin_state': emit('plugins:changed', null); break
      case 'interaction_change': emit('interactions:changed', event.payload); break
    }
  }
}
