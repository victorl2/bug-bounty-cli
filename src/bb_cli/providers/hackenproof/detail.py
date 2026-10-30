"""Readable detail pages for HackenProof programs and audit programs, in the shared document style."""
from __future__ import annotations

from bb_cli.evm.render import Doc, Style, date, joined, money, relative
from ..common import iso, links, rows_table

SEVERITIES = ('critical', 'high', 'medium', 'low')


def _header(doc: Doc, record: dict) -> None:
    doc.raw(doc.style.bold(record.get('title') or record.get('slug', '')) + '  ' + doc.style.link(record.get('url') or ''))


def _number(value) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def _range(low, high) -> str:
    low, high = _number(low), _number(high)
    if high <= 0:
        return ''
    return f'{money(low)} – {money(high)}' if low > 0 and low != high else money(high)


def overview(doc: Doc, record: dict) -> None:
    from .api import status
    s = doc.style
    _header(doc, record)
    labels = record.get('labels') or {}
    company = record.get('company') or {}
    end = None if record.get('unending_program') else iso(record.get('end_date'))
    sla = record.get('sla') or {}
    kyc = 'required' if record.get('kyc_required') else 'required for payment' if record.get('kyc_required_for_payment') else 'not required'
    doc.fields([
        ('Status', s.status(status(record).upper())),
        ('Max bounty', money(_number(record.get('max_bounty')))),
        ('Min bounty', money(_number(record.get('min_bounty')))),
        ('Starts', date(iso(record.get('start_date')))),
        ('Ends', f'{date(end)}  {s.dim(relative(end))}' if end else 'ongoing' if record.get('unending_program') else ''),
        ('KYC', kyc),
        ('PoC', 'required' if record.get('poc_required') else ''),
        ('Company', company.get('company_name') or ''),
        ('Triage', record.get('managed_by_company_name') or ''),
        ('Reports', f"{record['submitted_reports']:,} submitted" + (f", {record['valid_reports_count']:,} valid" if record.get('valid_reports_count') else '')
                    if isinstance(record.get('submitted_reports'), int) else ''),
        ('Paid out', money(_number(record.get('total_rewards')))),
        ('Ecosystems', joined(labels.get('platforms'))),
        ('Languages', joined(labels.get('languages'))),
        ('Asset types', joined(labels.get('types'))),
        ('Project types', joined(labels.get('project_types'))),
        ('Response SLA', f"triage {sla['time_to_triage']}d · reward {sla['time_to_reward']}d · resolution {sla['resolution_time']}d"
                         if all(k in sla for k in ('time_to_triage', 'time_to_reward', 'resolution_time')) else ''),
        ('Repository', record.get('repo_url') or ''),
    ])


def information(doc: Doc, record: dict) -> None:
    doc.section('Information')
    doc.text(record.get('desc'))
    rewards = record.get('rewards') or {}
    rows = [(sev.capitalize(), _range(rewards.get(f'{sev}_min'), rewards.get(f'{sev}_max'))) for sev in SEVERITIES]
    if any(v for _, v in rows):
        doc.sub('Rewards by severity')
        doc.fields(rows)
    for title, key in (('Focus area', 'focus_area'), ('Program rules', 'program_rules'),
                       ('Disclosure guidelines', 'disclosure_guidelines'), ('Eligibility', 'eligibility_and_coordinate_disclosure')):
        if record.get(key):
            doc.sub(title)
            doc.text(record[key])
    for block in record.get('custom_blocks') or []:
        doc.sub(block.get('title') or 'Notes')
        doc.text(block.get('description'))


def _scope_rows(scopes: list[dict]) -> list[list[str]]:
    rows = []
    for s in sorted(scopes, key=lambda s: s.get('position') or 0):
        notes = ' '.join(l.strip().lstrip('-* ').replace('**', '') for l in (s.get('target_description') or '').splitlines() if l.strip())
        rows.append([s.get('title') or '-', s.get('target') or '-', s.get('criticality') or '-', notes])
    return rows


def scope(doc: Doc, record: dict) -> None:
    doc.section('Scope')
    scopes = record.get('scopes') or []
    included = [s for s in scopes if not s.get('out_of_scope')]
    excluded = [s for s in scopes if s.get('out_of_scope')]
    if included:
        doc.sub(f'In scope ({len(included)})')
        rows_table(doc, ['NAME', 'TARGET', 'SEVERITY', 'NOTES'], _scope_rows(included), 3)
    if excluded:
        doc.sub(f'Out of scope ({len(excluded)})')
        rows_table(doc, ['NAME', 'TARGET', 'SEVERITY', 'NOTES'], _scope_rows(excluded), 3)
    if not scopes:
        doc.raw(doc.style.dim('  No asset list published; the information section describes the scope.'))


def resources(doc: Doc, record: dict) -> None:
    doc.section('Resources')
    doc.fields([
        ('Repository', record.get('repo_url') or ''),
        ('Pool', record.get('pool_url') or ''),
        ('Final report', record.get('final_report_url') or ''),
    ])
    found = links(record.get('desc'), record.get('focus_area'), record.get('program_rules'),
                  *(b.get('description') for b in record.get('custom_blocks') or []),
                  *(s.get('target') for s in record.get('scopes') or [] if not s.get('out_of_scope')))
    if found:
        doc.sub(f'Links ({len(found)})')
        doc.raw('\n'.join('  ' + doc.style.link(l) for l in found))


def leaderboard(doc: Doc, record: dict) -> None:
    doc.section('Leaderboard')
    ranks = record.get('hacker_ranks') or []
    if not ranks:
        doc.raw(doc.style.dim('  No ranked hackers yet.'))
        return
    rows = [[str(r.get('rank', '-')), (r.get('user') or {}).get('nickname') or '-', str(r.get('reports_amount') or 0), str(r.get('reputation') or 0)] for r in ranks]
    rows_table(doc, ['#', 'HACKER', 'REPORTS', 'REPUTATION'], rows, 1)
    total = record.get('hacker_ranks_count')
    if isinstance(total, int) and total > len(ranks):
        doc.raw(doc.style.dim(f'  top {len(ranks)} of {total} ranked hackers'))


SECTIONS = {'overview': overview, 'information': information, 'scope': scope, 'resources': resources, 'leaderboard': leaderboard}


def render(record: dict, sections: list[str], style: Style) -> str:
    doc = Doc(style)
    names = list(SECTIONS) if 'all' in sections else sections
    if 'overview' not in names:
        _header(doc, record)
    for name in names:
        SECTIONS[name](doc, record)
    return doc.render()
