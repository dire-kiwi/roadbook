## OnlyRoot

`packages/viewer/test/charts/edges.road:12`

````text
A root with rows of its own, wider than everything inside it.
```
a fence in a doc, then <b>HTML</b> & an ending fence:
```
````

```mermaid
stateDiagram-v2
  [*] --> OnlyRoot_machine
  state "OnlyRoot" as OnlyRoot_machine
  state OnlyRoot_machine {
    [*] --> S
    S --> S: Go
  }
  note right of OnlyRoot_machine
    entry do note("x")
    exit do note("y")
    on Go / do note("a root internal transition with a long text, wider than the state inside")
  end note
```

## Note

`packages/viewer/test/charts/edges.road:22`

```text
States Mermaid would read as keywords.
```

```mermaid
stateDiagram-v2
  state "State" as State_
  state "Direction" as Direction_
  state "ClassDef" as ClassDef_
  state "Class" as Class_
  state "Style" as Style_
  state "Default" as Default_
  state "Click" as Click_
  state "Href" as Href_
  state "Scale" as Scale_
  state "As" as As_
  state "AccTitle" as AccTitle_
  state "AccDescr" as AccDescr_
  state "StateDiagram" as StateDiagram_
  [*] --> State_
  State_ --> Direction_: Go
  Direction_ --> ClassDef_: Go
  ClassDef_ --> Class_: Go
  Class_ --> Style_: Go
  Style_ --> Default_: Go
  Default_ --> Click_: Go
  Click_ --> Href_: Go
  Href_ --> Scale_: Go
  Scale_ --> As_: Go
  As_ --> AccTitle_: Go
  AccTitle_ --> AccDescr_: Go
  AccDescr_ --> StateDiagram_: Go
  StateDiagram_ --> [*]
```

## Same

`packages/viewer/test/charts/edges.road:41`

```text
A state named like its machine, which has transitions of its own.
```

```mermaid
stateDiagram-v2
  [*] --> Same_machine
  state "Same" as Same_machine
  state Same_machine {
    [*] --> Same
    Same --> Bar: Go
    Bar --> [*]
  }
  Same_machine --> Same: Go
```

## Rows

`packages/viewer/test/charts/edges.road:50`

```text
Containers with an invoke, entry, exit and internal transitions, and labels with syntax.
```

```mermaid
stateDiagram-v2
  state Busy {
    state "Inner" as Busy_Inner
    state Busy_Inner {
      state "Deep" as Busy_Inner_Deep
      state "Deeper" as Busy_Inner_Deeper
      [*] --> Busy_Inner_Deep
      Busy_Inner_Deep --> Busy_Inner_Deeper: Go
      Busy_Inner_Deeper --> Busy_Inner_Deep: Go
    }
    [*] --> Busy_Inner
  }
  note right of Busy
    invoke work(n)
    entry do note("entering Busy, which is a container with a fairly long entry row")
    exit set n = n + 1
    on Bump / do note("#100;irection TB#59; a#58;#58;b #35;c#58; %% d")
  end note
  [*] --> Busy
  Busy --> Done: done
  Done --> [*]
```
