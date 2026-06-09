// Admin pane: log in, then manage the node inventory and encrypted credentials
// (Workstreams D + E). Mutations carry the bearer token; reads stay open. In skeleton
// mode the API returns 503 (admin_unavailable) and the pane shows a hint.

import { useEffect, useState } from 'react';
import type { ChangeEvent, FormEvent } from 'react';
import { api, ApiError } from '../../services/api';
import { useAuthStore } from '../../store';
import type { CredentialSummary, NodeSummary } from '../../types/api';
import { Login } from '../Login/Login';

export function Admin() {
  const authed = useAuthStore((s) => s.authed);
  const setAuthed = useAuthStore((s) => s.setAuthed);

  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [creds, setCreds] = useState<CredentialSummary[]>([]);
  const [available, setAvailable] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [name, setName] = useState('');
  const [address, setAddress] = useState('');
  const [credName, setCredName] = useState('');
  const [credKind, setCredKind] = useState('snmp_v2c');
  const [credSecret, setCredSecret] = useState('');

  const load = () => {
    api.listNodes().then(setNodes).catch(() => undefined);
    api
      .listCredentials()
      .then((c) => {
        setCreds(c);
        setAvailable(true);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setAvailable(false);
      });
  };
  useEffect(() => {
    if (authed) load();
  }, [authed]);

  const onError = (e: unknown) => setError(e instanceof ApiError ? e.message : 'request failed');
  const text = (set: (v: string) => void) => (e: ChangeEvent<HTMLInputElement>) =>
    set(e.target.value);

  const logout = () => {
    api.logout();
    setAuthed(false);
  };

  const addNode = (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    api
      .createNode({ name, address })
      .then(() => {
        setName('');
        setAddress('');
        load();
      })
      .catch(onError);
  };

  const addCredential = (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    api
      .createCredential({ name: credName, kind: credKind, secret: credSecret })
      .then(() => {
        setCredName('');
        setCredSecret('');
        load();
      })
      .catch(onError);
  };

  if (!authed) {
    return <Login />;
  }

  if (!available) {
    return (
      <section className="pane">
        <h2>Admin</h2>
        <p className="muted">Inventory / credential management is unavailable in skeleton mode.</p>
        <button className="node-tab" onClick={logout}>
          Log out
        </button>
      </section>
    );
  }

  return (
    <section className="pane">
      <div className="admin-row">
        <h2 className="admin-grow">Inventory</h2>
        <button className="node-tab" onClick={logout}>
          Log out
        </button>
      </div>
      <form className="admin-form" onSubmit={addNode}>
        <input placeholder="name" value={name} onChange={text(setName)} />
        <input placeholder="IP address" value={address} onChange={text(setAddress)} />
        <button className="node-tab" type="submit">
          Add node
        </button>
      </form>
      {error && <p className="muted">{error}</p>}
      {nodes.map((n) => (
        <div className="admin-row" key={n.id}>
          <span className="admin-grow">
            {n.name} <span className="muted">{n.address}</span>
          </span>
          <button className="node-tab" onClick={() => api.deleteNode(n.id).then(load).catch(onError)}>
            Delete
          </button>
        </div>
      ))}

      <h2 className="admin-section">Credentials</h2>
      <form className="admin-form" onSubmit={addCredential}>
        <input placeholder="name" value={credName} onChange={text(setCredName)} />
        <select value={credKind} onChange={(e) => setCredKind(e.target.value)}>
          <option value="snmp_v2c">snmp_v2c</option>
          <option value="snmp_v3">snmp_v3</option>
          <option value="api_token">api_token</option>
        </select>
        <input
          placeholder="secret"
          type="password"
          value={credSecret}
          onChange={text(setCredSecret)}
        />
        <button className="node-tab" type="submit">
          Add credential
        </button>
      </form>
      {creds.map((c) => (
        <div className="admin-row" key={c.id}>
          <span className="admin-grow">
            {c.name} <span className="muted">({c.kind})</span>
          </span>
          <button
            className="node-tab"
            onClick={() => api.deleteCredential(c.id).then(load).catch(onError)}
          >
            Delete
          </button>
        </div>
      ))}
    </section>
  );
}
