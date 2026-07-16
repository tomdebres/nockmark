::  nockmark registry kernel: challenges + cryptographically-gated runs.
::  The DRIVER verifies proofs; %submit-run only records post-verification.
/=  *  /common/zoon
/=  *  /common/wrapper
=<  ((moat |) inner)  :: wrapped kernel
=>
|%
+$  challenge  [issued-at=@da used=?]
+$  run
  $:  id=@
      nonce=@
      hardware=@t
      prover-version=@t
      k=@
      elapsed-ms=@
      issued-at=@da
      submitted-at=@da
  ==
+$  registry-state
  $:  %0
      challenges=(map @ challenge)
      runs=(list run)
      next-id=@
  ==
+$  cause
  $%  [%new-challenge ~]
      [%submit-run nonce=@ hardware=@t prover-version=@t k=@ elapsed-ms=@]
  ==
+$  effect
  $%  [%challenge-minted nonce=@]
      [%run-recorded id=@]
      [%rejected reason=@t]
  ==
--
|%
++  moat  (keep registry-state)
++  inner
  |_  k=registry-state
  ++  load  |=(s=registry-state s)
  ++  peek
    |=  arg=path
    ^-  (unit (unit *))
    ?+  arg  ~
        [%leaderboard ~]
      ``runs.k
        [%runs @ ~]
      =/  id  (slav %ud i.t.arg)
      ``(skim runs.k |=(r=run =(id.r id)))
    ==
  ++  poke
    |=  [wir=wire eny=@ our=@ux now=@da dat=*]
    ^-  [(list effect) registry-state]
    =/  soft-cau  ((soft cause) dat)
    ?~  soft-cau
      [[%rejected 'bad-cause']~ k]
    ?-    -.u.soft-cau
        %new-challenge
      =/  nonce=@  (end 6 eny)              ::  64-bit nonce from entropy
      :-  [%challenge-minted nonce]~
      k(challenges (~(put by challenges.k) nonce [now |]))
    ::
        %submit-run
      =/  cau  u.soft-cau
      =/  c  (~(get by challenges.k) nonce.cau)
      ?~  c
        [[%rejected 'unknown-nonce']~ k]
      ?:  used.u.c
        [[%rejected 'nonce-used']~ k]
      ?:  (gth now (add issued-at.u.c ~h1))
        [[%rejected 'stale-nonce']~ k]
      =/  window-ms  (div (mul (sub now issued-at.u.c) 1.000) ~s1)
      ?:  (gth elapsed-ms.cau window-ms)
        [[%rejected 'elapsed-exceeds-window']~ k]
      =/  r=run
        :*  next-id.k  nonce.cau  hardware.cau  prover-version.cau
            k.cau  elapsed-ms.cau  issued-at.u.c  now
        ==
      :-  [%run-recorded next-id.k]~
      %_  k
        runs        [r runs.k]
        next-id     +(next-id.k)
        challenges  (~(put by challenges.k) nonce.cau [issued-at.u.c &])
      ==
    ==
  --
--
