/**
 * FEATURE FLAGS: the index.
 *
 * The row is the useful unit here: the key your code reads, what the flag is
 * for, whether it is serving, and how far the rollout has actually been pushed,
 * drawn as the same band the editor uses so the two never disagree. Opening a
 * flag brings the editor up underneath, keeping the list in view.
 */
import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { Button, Empty, Panel, PanelHead } from "../../components/react/ui";
import { IconPlus, IconSearch, IconSettings } from "../../components/react/icons";
import { ago, num } from "../../lib/format";
import { useSite } from "../providers";
import { Reading, TextInput, Throw, errText } from "./QueryScreen";
import {
  FlagEditor,
  RolloutBand,
  blankFlag,
  effectiveVariants,
  flagsKey,
  normaliseFlag,
} from "./FlagEditor";
import type { FeatureFlag } from "./FlagEditor";

export function FlagsScreen() {
  const { siteId, isLoading: siteLoading } = useSite();
  const qc = useQueryClient();

  const [open, setOpen] = useState<FeatureFlag | null>(null);
  const [find, setFind] = useState("");

  const q = useQuery({
    queryKey: flagsKey(siteId),
    enabled: siteId != null,
    queryFn: async () => {
      const res = await api.get<FeatureFlag[]>("/api/flags", { site: siteId as number });
      return (res.data ?? []).map((f) => normaliseFlag(f as unknown as Record<string, unknown>));
    },
  });

  const toggle = useMutation({
    mutationFn: (input: { flag: FeatureFlag; enabled: boolean }) =>
      api.patch<FeatureFlag>(`/api/flags/${input.flag.id}`, { enabled: input.enabled }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: flagsKey(siteId) }),
  });

  const all = q.data ?? [];
  const needle = find.trim().toLowerCase();
  const flags = useMemo(
    () =>
      needle
        ? all.filter(
            (f) =>
              f.key.toLowerCase().includes(needle) || f.description.toLowerCase().includes(needle),
          )
        : all,
    [all, needle],
  );

  return (
    <div className="mf-bn-page">
      <Panel>
        <PanelHead
          title="Feature flags"
          note="A flag is a key your code reads at runtime. Turn it on, roll it out to a share of visitors, or split that share into variants."
          actions={
            <Button
              tone="primary"
              icon={<IconPlus size={13} />}
              onClick={() => setOpen(blankFlag(siteId))}
            >
              New flag
            </Button>
          }
        />

        {all.length > 6 ? (
          <div className="mf-bn-find">
            <IconSearch size={14} />
            <TextInput
              value={find}
              onChange={setFind}
              placeholder="Find a flag by key or description"
              ariaLabel="Find a flag"
            />
            {needle ? (
              <span className="mf-bn-find__count mf-data">
                {num(flags.length)} of {num(all.length)}
              </span>
            ) : null}
          </div>
        ) : null}

        <div className="mf-bn-indexwrap">
          <Reading
            loading={siteLoading || q.isLoading}
            error={q.isError ? q.error : undefined}
            onRetry={() => void q.refetch()}
            isEmpty={flags.length === 0}
            lines={4}
            empty={
              all.length && needle ? (
                <Empty
                  title="No flag matches that"
                  body="Nothing here has that in its key or its description."
                  action={<Button onClick={() => setFind("")}>Show every flag</Button>}
                />
              ) : (
                <Empty
                  title="No flags yet"
                  body="A flag lets you ship code that is off, turn it on for a share of visitors, and turn it back off without a deploy. Exposures are recorded against real events, so you can see what each variant actually did."
                  action={
                    <Button
                      tone="primary"
                      icon={<IconPlus size={13} />}
                      onClick={() => setOpen(blankFlag(siteId))}
                    >
                      New flag
                    </Button>
                  }
                />
              )
            }
          >
            <ul className="mf-bn-flags">
              {flags.map((f) => {
                const vs = effectiveVariants(f);
                const pending = toggle.isPending && toggle.variables?.flag.id === f.id;
                return (
                  <li
                    key={f.id}
                    className={`mf-bn-flags__row${open?.id === f.id ? " is-open" : ""}`}
                  >
                    <div className="mf-bn-flags__ident">
                      <span className="mf-bn-flags__key mf-data">{f.key}</span>
                      <p className="mf-bn-flags__desc">
                        {f.description || "No description. Open it and say what it is for."}
                      </p>
                      <p className="mf-bn-flags__meta">
                        {f.variants.length
                          ? `${f.variants.length} variants: ${vs.map((v) => v.key).join(", ")}`
                          : "On or off"}
                        {f.conditions.length
                          ? ` · ${f.conditions.length} targeting condition${f.conditions.length === 1 ? "" : "s"}`
                          : " · no targeting"}
                        {f.updated_at ? ` · changed ${ago(f.updated_at)}` : ""}
                      </p>
                    </div>

                    <div className="mf-bn-flags__band">
                      <RolloutBand flag={f} compact />
                    </div>

                    <div className="mf-bn-flags__act">
                      <Throw
                        label={`Serving ${f.key}`}
                        on={f.enabled}
                        onChange={(v) => {
                          if (!pending) toggle.mutate({ flag: f, enabled: v });
                        }}
                      />
                      <Button
                        size="sm"
                        icon={<IconSettings size={13} />}
                        onClick={() => setOpen(open?.id === f.id ? null : f)}
                      >
                        {open?.id === f.id ? "Close" : "Open"}
                      </Button>
                    </div>
                  </li>
                );
              })}
            </ul>
          </Reading>

          {toggle.isError ? (
            <p className="mf-bn-field__err" role="alert">
              That flag was not changed. {errText(toggle.error)}
            </p>
          ) : null}
        </div>
      </Panel>

      {open ? (
        <FlagEditor
          key={open.id || "new"}
          flag={open}
          onDone={(result) => {
            setOpen(null);
            if (result === "saved" || result === "deleted") {
              void qc.invalidateQueries({ queryKey: flagsKey(siteId) });
            }
          }}
        />
      ) : null}
    </div>
  );
}

export default FlagsScreen;
