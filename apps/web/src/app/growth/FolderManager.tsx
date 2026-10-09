/**
 * Link folders: create, rename, delete.
 *
 * Folders belong to the organisation, not to a site, so the same list appears
 * whichever site the operator is reading, and the manager says so. It opens
 * inside the links panel, under the toolbar, rather than in a dialog: it is a
 * short list that is managed while looking at the links it sorts.
 *
 * Deleting a folder never deletes a link. The links move to "No folder", and
 * the confirmation says how many will move before anything happens.
 */
import type { SubmitEvent } from "react";
import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { Button } from "../../components/react/ui";
import { IconCheck, IconPen, IconPlus } from "../../components/react/icons";
import { api } from "../../lib/api";
import { num } from "../../lib/format";
import { Say, TextInput, WaitLines } from "./controls";
import { problem, scopeQuery } from "./model";
import type { Folder, LinkScope } from "./model";

function FolderRow({ folder, onDeleted }: { folder: Folder; onDeleted: (id: string) => void }) {
  const qc = useQueryClient();
  const [mode, setMode] = useState<"idle" | "rename" | "delete">("idle");
  const [name, setName] = useState(folder.name);

  const rename = useMutation({
    mutationFn: (next: string) =>
      api.patch<Folder>(`/api/link-folders/${encodeURIComponent(folder.id)}`, { name: next }),
    onSuccess: () => {
      setMode("idle");
      void qc.invalidateQueries({ queryKey: ["links"] });
    },
  });
  const remove = useMutation({
    mutationFn: () => api.del<{ deleted: boolean }>(`/api/link-folders/${encodeURIComponent(folder.id)}`),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["links"] });
      onDeleted(folder.id);
    },
  });

  const submitRename = (e: SubmitEvent) => {
    e.preventDefault();
    const next = name.trim();
    if (!next || next === folder.name) {
      setMode("idle");
      return;
    }
    rename.mutate(next);
  };

  const links = `${num(folder.links)} ${folder.links === 1 ? "link" : "links"}`;

  return (
    <li className="mf-gr-folder">
      {mode === "rename" ? (
        <form className="mf-gr-folder__form" onSubmit={submitRename}>
          <TextInput
            aria-label={`New name for the folder ${folder.name}`}
            value={name}
            maxLength={80}
            autoFocus
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setName(folder.name);
                setMode("idle");
              }
            }}
          />
          <Button type="submit" size="sm" icon={<IconCheck size={13} />} disabled={rename.isPending}>
            {rename.isPending ? "Saving…" : "Save"}
          </Button>
          <Button
            size="sm"
            tone="quiet"
            onClick={() => {
              setName(folder.name);
              setMode("idle");
            }}
          >
            Cancel
          </Button>
        </form>
      ) : (
        <div className="mf-gr-folder__line">
          <span className="mf-gr-folder__name">{folder.name}</span>
          <span className="mf-gr-folder__count mf-data">{links}</span>
          <span className="mf-gr-folder__acts">
            <Button size="sm" tone="quiet" icon={<IconPen size={12} />} onClick={() => setMode("rename")}>
              Rename
            </Button>
            <Button size="sm" tone="quiet" onClick={() => setMode("delete")}>
              Delete
            </Button>
          </span>
        </div>
      )}

      {mode === "delete" ? (
        <div className="mf-gr-ask" role="group" aria-label={`Confirm deleting the folder ${folder.name}`}>
          <p className="mf-gr-ask__text">
            Delete the folder “{folder.name}”?{" "}
            {folder.links > 0
              ? `Its ${links} move to No folder. No link is deleted.`
              : "It holds no links."}
          </p>
          <div className="mf-gr-ask__acts">
            <Button tone="danger" size="sm" disabled={remove.isPending} onClick={() => remove.mutate()}>
              {remove.isPending ? "Deleting…" : "Delete folder"}
            </Button>
            <Button size="sm" onClick={() => setMode("idle")}>
              Keep it
            </Button>
          </div>
        </div>
      ) : null}

      {rename.isError ? <Say tone="warn">{problem(rename.error)}</Say> : null}
      {remove.isError ? <Say tone="warn">{problem(remove.error)}</Say> : null}
    </li>
  );
}

export function FolderManager({
  scope,
  folders,
  loading,
  error,
  onRetry,
  onDeleted,
}: {
  scope: LinkScope;
  folders: Folder[];
  loading: boolean;
  error: unknown;
  onRetry: () => void;
  onDeleted: (id: string) => void;
}) {
  const qc = useQueryClient();
  const [name, setName] = useState("");

  const create = useMutation({
    mutationFn: (next: string) => api.post<Folder>(`/api/link-folders?${scopeQuery(scope)}`, { name: next }),
    onSuccess: () => {
      setName("");
      void qc.invalidateQueries({ queryKey: ["links"] });
    },
  });

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    const next = name.trim();
    if (next) create.mutate(next);
  };

  return (
    <section className="mf-gr-folders" aria-labelledby="mf-gr-folders-title">
      <div className="mf-gr-folders__head">
        <h3 id="mf-gr-folders-title" className="mf-gr-folders__title">
          Folders
        </h3>
        <p className="mf-gr-folders__note">
          Folders belong to the organisation, so every site in it sees the same folders.
        </p>
      </div>

      {loading ? (
        <WaitLines lines={2} label="Loading folders…" />
      ) : error ? (
        <div className="mf-gr-inline">
          <Say tone="warn">Could not load the folders. {problem(error)}</Say>
          <Button size="sm" onClick={onRetry}>
            Try again
          </Button>
        </div>
      ) : folders.length ? (
        <ul className="mf-gr-folders__list">
          {folders.map((f) => (
            <FolderRow key={f.id} folder={f} onDeleted={onDeleted} />
          ))}
        </ul>
      ) : (
        <p className="mf-gr-folders__none">No folders yet. A folder groups links, for example by campaign.</p>
      )}

      <form className="mf-gr-folders__new" onSubmit={submit}>
        <TextInput
          aria-label="Name of the new folder"
          placeholder="New folder name"
          maxLength={80}
          value={name}
          onChange={(e) => {
            setName(e.target.value);
            if (create.isError) create.reset();
          }}
        />
        <Button type="submit" size="sm" icon={<IconPlus size={13} />} disabled={!name.trim() || create.isPending}>
          {create.isPending ? "Adding…" : "Add folder"}
        </Button>
      </form>
      {create.isError ? <Say tone="warn">{problem(create.error)}</Say> : null}
    </section>
  );
}
