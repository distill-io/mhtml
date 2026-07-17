// Format verification results into a ranked, human-scannable summary. Hard
// fails come first, then passes closest to a threshold (the near-misses most
// worth distilling into a regression test), then clean passes.

function severity(r) {
  if (r.status === "capture-error" || r.status === "extract-error" || r.status === "render-error") {
    return 3;
  }
  if (r.status === "fail") return 2;
  return 0;
}

// Lower text-dice = closer to failing = ranked earlier among passes.
function margin(r) {
  return r.compare ? r.compare.checks.text.score : 1;
}

export function rankResults(results) {
  return [...results].sort((a, b) => {
    const s = severity(b) - severity(a);
    if (s !== 0) return s;
    return margin(a) - margin(b);
  });
}

export function formatSummary(results) {
  const ranked = rankResults(results);
  const pass = results.filter((r) => r.status === "pass").length;
  const fail = results.filter((r) => r.status === "fail").length;
  const errored = results.length - pass - fail;

  const lines = [];
  lines.push(`Conservation: ${results.length} archives — ${pass} pass, ${fail} fail, ${errored} error`);
  lines.push("");
  for (const r of ranked) {
    if (r.status === "pass") {
      const t = r.compare.checks.text.score;
      lines.push(`  PASS  ${r.id.padEnd(28)} text=${t}`);
    } else if (r.status === "fail") {
      lines.push(`  FAIL  ${r.id.padEnd(28)} ${r.compare.diffs.join("; ")}`);
    } else {
      lines.push(`  ERR   ${r.id.padEnd(28)} ${r.status}: ${r.error || ""}`);
    }
  }
  return lines.join("\n");
}
