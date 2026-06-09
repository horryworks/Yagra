// Sign-in form, shared by the app-level gate (private-dashboard mode) and the Admin pane
// (public mode, log in to manage). On success it stores the bearer token (in the api
// client) and flips the shared auth state so both consumers re-render.

import { useState } from 'react';
import type { ChangeEvent, FormEvent } from 'react';
import { api, ApiError } from '../../services/api';
import { useAuthStore } from '../../store';

export function Login({ title = 'Sign in' }: { title?: string }) {
  const setAuthed = useAuthStore((s) => s.setAuthed);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);

  const text = (set: (v: string) => void) => (e: ChangeEvent<HTMLInputElement>) =>
    set(e.target.value);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    api
      .login(username, password)
      .then(() => {
        setPassword('');
        setAuthed(true);
      })
      .catch((x: unknown) => setError(x instanceof ApiError ? x.message : 'login failed'));
  };

  return (
    <section className="pane">
      <h2>{title}</h2>
      <form className="admin-form" onSubmit={submit}>
        <input placeholder="username" value={username} onChange={text(setUsername)} />
        <input
          placeholder="password"
          type="password"
          value={password}
          onChange={text(setPassword)}
        />
        <button className="node-tab" type="submit">
          Log in
        </button>
      </form>
      {error && <p className="muted">{error}</p>}
    </section>
  );
}
