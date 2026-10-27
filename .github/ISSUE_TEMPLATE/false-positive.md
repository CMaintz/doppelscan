---
name: Wrong match or missed clone
about: doppelscan reported code that is not a real duplicate, or missed one that is
labels: matching
---

**What happened**
Reported as a clone but should not be / should be a clone but was not reported.

**Snippets**
The smallest self-contained pair of files that shows it:

```
// file a
```

```
// file b
```

**Command and output**

```
$ doppelscan ...
```

**Version**
`doppelscan --version`:
