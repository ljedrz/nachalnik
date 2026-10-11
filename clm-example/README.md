# a CLM-style context edit on nachalnik

kamchatka built from commit [`fe8f0532`](https://github.com/ljedrz/nachalnik/commit/fe8f053259804a165d7b9a2f13a706250e27c887), running `anthropic/claude-haiku-5.5` through OpenRouter. The model's only tools are `fs` and its own `context`. There's no system prompt, and kamchatka's compactor is off (`--compact 1`), so the one edit below is the model's own. Numbers in brackets are context item ids.

```console
$ kamchatka --headless --config-file kamchatka.json --compact 1 -m anthropic/claude-haiku-5.5 < script.txt > session.jsonl
```

```json
{ "tools": ["fs", "context"], "allow": ["fs:read", "fs:glob", "context"], "on-ask": "deny", "requests": 20, "spend": 400000, "deadline": 900 }
```

## turn 1: search

**user [1]**

> In what year was the lighthouse keeper who made the first recorded sighting of the green flash at Skerryvane born? The files in results/round1/ are search results already fetched for you; read them with fs and tell me what you found. Don't answer yet.

**model [2], [4]**: `fs` glob `results/round1/*`, then reads the three files

<details><summary>[5]-[7]: the three search results</summary>

```text
== doc-0932.txt ==
The Green Flash Explained

The green flash is seen as the last rim of the sun drops below a sharp horizon. Refraction bends
green light more than red, so the green image of the sun sets last. It lasts about a second, and
the open sea is the best place to look for it.

== doc-4471.txt ==
Skerryvane Light - A Station History (Northern Lights Heritage Trust)

The light was first exhibited in 1871. The station journal entry of 14 June 1884 records "a flash
of clear green at the instant the sun's limb went under" - the earliest known description of the
green flash in a Scottish station journal. The keeper on watch was Assistant Keeper Tomas Venn,
who left the service in 1891. For his family, see the Orkney parish registers.

== doc-7710.txt ==
Skerryvane Sea Tours

Four-hour trips around Skerryvane lighthouse leave Port Kell at 10:00, May to September. Puffins
nest on the skerries until August. Our Friday sunset cruises are popular with guests hoping to
see the green flash. Adults 48 pounds, children 24 pounds.
```

</details>

**model [8]**

> I've read all three files in `results/round1/`. They don't give the keeper's birth year.
>
> - **doc-4471.txt** (Skerryvane Light station history, Northern Lights Heritage Trust) is the relevant one. It says the light was first exhibited in 1871. The station journal entry of 14 June 1884 records "a flash of clear green at the instant the sun's limb went under," described as the earliest known green flash in a Scottish station journal. The keeper on watch was Assistant Keeper Tomas Venn, who left the service in 1891. It has no birth date for him. It points to the Orkney parish registers for his family, but those aren't in the folder.
> - **doc-0932.txt** is a general explainer on the green flash. It doesn't mention Skerryvane or any keeper.
> - **doc-7710.txt** is a tour operator's advertisement. It mentions Skerryvane sunset cruises but nothing about the keeper or the sighting.
>
> So the chain is: first recorded sighting, 14 June 1884, by Assistant Keeper Tomas Venn. The birth year is still missing. The next step would be the Orkney parish registers or another source on Venn's birth, which aren't in these files. I haven't given a year, as you asked.

## turn 2: the model rewrites its context

**user [9]**

> Your context is yours to edit with the context tool. Compact it now, keeping only your key findings and current plan. For example: revise your previous reply to hold just the findings and the plan, and exclude the search results.

**model [10]**: `context` · `look`, which lists the 10 items and what each costs

**model [12]**: `context` · `revise` [8], reason: "Compacting as asked: keeping only key findings and the plan, dropping the raw search text."

```text
Findings so far:
- The first recorded green flash at Skerryvane is in the station journal entry of 14 June 1884 ("a flash of clear green at the instant the sun's limb went under"). Source: Northern Lights Heritage Trust station history of Skerryvane Light.
- The keeper on watch was Assistant Keeper Tomas Venn, who left the service in 1891.
- The source gives no birth date for Venn. It points to the Orkney parish registers for his family.
- The other two files (a general green flash explainer and a tour operator ad) don't name the keeper.

Plan: find Tomas Venn's birth year in the Orkney parish registers or another source on him. No birth year has been found, so no answer has been given yet.
```

`context` · `exclude` [3, 5, 6, 7], reason: "Search results already summarised in my notes; the raw text is no longer needed."

**model [15]**

> I compacted the context. The findings and plan are now held in my previous reply (item 8), and the raw search results (items 3, 5, 6, 7) are excluded from the next request. Restoring them is one step if I need them.
>
> The question is still open. The station history gives the first green flash as the 14 June 1884 journal entry, with Assistant Keeper Tomas Venn on watch. It doesn't give his birth year, so the next step is to find his birth year in the Orkney parish registers or another source on him. I haven't given an answer yet.

The next request (seq 96 in the log) carried items `[1, 8, 9, …]`, so turn 1 went out as the question plus the rewritten [8]. The calls in [2] and [4] were left out because their results are excluded.

## turn 3: the next step runs on the rewritten context

**user [16]**

> Now read results/round2/ and answer the question.

**model [17], [19]**: `fs` glob `results/round2/*`, then reads `doc-2205.txt`

<details><summary>[20]: the search result</summary>

```text
Orkney Family History Society - Index of Lighthouse Families

VENN, Tomas. Assistant lightkeeper 1876-1891. Born 3 March 1853 at Quoyness, Stromness, Orkney.
Not to be confused with his cousin Thomas Venn, born 1849 in Kirkwall, a merchant seaman.
```

</details>

**model [21]**

> Tomas Venn, the assistant keeper on watch when the green flash was first recorded at Skerryvane, was born in **1853**. The Orkney Family History Society index gives his birth as 3 March 1853 at Quoyness, Stromness, and his service as 1876–1891, which matches the station history's note that he left in 1891.
>
> The index also lists a cousin, Thomas Venn, born 1849 in Kirkwall. He is a different person, so I haven't used his date.

## what the record keeps

Nothing was deleted. The log holds what [8] said before (`context.replaced`, seq 83) and each exclusion with the model's reason (`context.changed`, seq 88-91). `undo` puts them back, and `kamchatka --check` finds the record consistent.

## the files

- `2026-10-11T05-34-47Z.jsonl` is the session log, one event per line, and `2026-10-11T05-34-47Z.json` is the snapshot of the context at the end. `kamchatka -r 2026-10-11T05-34-47Z.json` continues the session.
- `script.txt` is the input the run read: three messages and a `/save`.
- `kamchatka.json` is the settings file shown above.
- `results/` holds the made-up search results the model read.
