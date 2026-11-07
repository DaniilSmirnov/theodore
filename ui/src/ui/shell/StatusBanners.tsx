/**
 * What is broken right now, in a strip under the page — one banner at a
 * time, most urgent first. Offline wins over auth, which wins over rate
 * limiting, which wins over a host answering with server errors: there is
 * no point telling someone their token is bad while the network itself is
 * down. A 502 is not offline: the host answered, it is having trouble.
 */
import { useEffect, useState } from 'react';
import { Alert, Button } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { HostStatus } from '../../stores/EphemeralStore';
import type { SyncStatus } from '../../sync/SyncClient';
import { useRootStore, useUi } from '../../stores/context';
import { Spinner } from '../bits';
import { refreshInbox } from '../../actions';

export type StatusKind = 'offline' | 'auth' | 'rate' | 'trouble' | null;

/** Pure so it is cheap to unit-test: which banner (if any) applies right now. */
export function statusKind(
  syncStatus: SyncStatus,
  host: HostStatus,
  now: number,
): StatusKind {
  if (syncStatus === 'error' || !host.online) return 'offline';
  if (!host.authOk) return 'auth';
  if (host.rateLimitedUntil !== null && host.rateLimitedUntil > now) {
    return 'rate';
  }
  if (host.serverTrouble) return 'trouble';
  return null;
}

/** Time left until a reset: seconds under two minutes (82 s is "1 min
 *  22 s", not "2 min"), then whole minutes rounded, then hours. */
export function formatCountdown(ms: number): string {
  const secs = Math.max(0, Math.ceil(ms / 1000));
  if (secs < 60) return `${secs} s`;
  if (secs < 120) {
    const rest = secs - 60;
    return rest ? `1 min ${rest} s` : '1 min';
  }
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins} min`;
  const hrs = Math.floor(mins / 60);
  const rem = mins % 60;
  return rem ? `${hrs}h ${rem}m` : `${hrs}h`;
}

/** Re-renders once a second while mounted — just enough for a countdown. */
function useNow(activeMs: number | null): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (activeMs === null) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [activeMs]);
  return now;
}

export const StatusBanners = observer(function StatusBanners() {
  const store = useRootStore();
  const ui = useUi();
  const [retrying, setRetrying] = useState(false);
  const host = store.ephemeral.hostStatus;
  const now = useNow(host.rateLimitedUntil);
  const kind = statusKind(store.sync.status, host, now);

  if (kind === 'offline') {
    const pending = store.sync.queue.pending.length;
    // Whole in the tooltip when the strip is too narrow for it.
    const offlineText = `Reviews and spaces you’ve opened stay readable; syncing and agents resume when you’re back online. ${
      pending > 0
        ? `${pending} ${pending === 1 ? 'write is' : 'writes are'} queued and will go out once reconnected.`
        : 'Anything you do will be queued and sent automatically.'
    }`;
    return (
      <Alert status="warning" className="status-banner status-offline">
        <Alert.Indicator />
        <Alert.Content>
          <Alert.Title>Offline</Alert.Title>
          <Alert.Description title={offlineText}>
            {offlineText}
          </Alert.Description>
        </Alert.Content>
        <Button
          size="sm"
          variant="tertiary"
          className="retry-connection"
          isPending={retrying}
          onPress={() => {
            setRetrying(true);
            // Reconnect to the local core, then ask the host again: a read
            // that gets through is what clears an offline host.
            void store.sync
              .retry()
              .then(() => refreshInbox(store))
              .finally(() => setRetrying(false));
          }}
        >
          {retrying && <Spinner />}
          Retry
        </Button>
      </Alert>
    );
  }

  if (kind === 'auth') {
    return (
      <Alert status="danger" className="status-banner status-auth">
        <Alert.Indicator />
        <Alert.Content>
          <Alert.Title>GitHub sign-in failed</Alert.Title>
          <Alert.Description>
            The saved token was rejected — reconnect to keep writes going.
          </Alert.Description>
        </Alert.Content>
        <Button
          size="sm"
          variant="tertiary"
          className="reconnect-host"
          onPress={() => ui.go({ name: 'settings', page: 'account' })}
        >
          Reconnect
        </Button>
      </Alert>
    );
  }

  if (kind === 'rate') {
    return (
      <Alert status="warning" className="status-banner status-rate">
        <Alert.Indicator />
        <Alert.Content>
          <Alert.Title>GitHub rate limit reached</Alert.Title>
          <Alert.Description>
            Resets in {formatCountdown(host.rateLimitedUntil! - now)}. Reads
            fall back to cached data until then.
          </Alert.Description>
        </Alert.Content>
      </Alert>
    );
  }

  if (kind === 'trouble') {
    const text =
      'Some requests fail with server errors. Reads fall back to cached data; Theodore keeps retrying.';
    return (
      <Alert status="warning" className="status-banner status-trouble">
        <Alert.Indicator />
        <Alert.Content>
          <Alert.Title>The host is having trouble</Alert.Title>
          <Alert.Description title={text}>{text}</Alert.Description>
        </Alert.Content>
        <Button
          size="sm"
          variant="tertiary"
          className="retry-connection"
          isPending={retrying}
          onPress={() => {
            setRetrying(true);
            void refreshInbox(store).finally(() => setRetrying(false));
          }}
        >
          {retrying && <Spinner />}
          Retry
        </Button>
      </Alert>
    );
  }

  return null;
});
