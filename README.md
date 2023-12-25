# coco
Coco is a fun and hobbyist general-purpose systems programming language. It aims to create a robust and friendly development environment to achieve ease, speed, safety, and efficiency. Note: It's in an under-development state. As a learner, I also aim to improve and reach a better state.

### Lexical Grammer
```
"This is a comment"

name := constantValue

mut name := variableValue
name = mutated-variableValue

if condition { statement }
if condition? in { | caseCondition -> statement | caseCondition -> statement |  _  -> statement }

do condition { statement }
do init; condition { statement }
do { statement }
do name in num,num { statement }
do name in collection { statement }
do name,name in collection { statement }

name := @{ statement }

name := | val | val | ...

name := (arguments -> returnTypes) { statement }

name.name := (arguments -> returnTypes) { statement }

lib main  "package"
@lib std  "import"
```
