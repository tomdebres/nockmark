GPU spike pending AWS quota approval (case 178413272800144).
When approved, next session runs:
1. launch g6.2xlarge: ami-0692a1f31ac4650c8, sg-07e04a86898f1c1fd, key nockmark-bench, 60GB gp3, us-east-1
2. aws ec2 wait instance-status-ok
3. scp tock/gpu-spike.sh; bash gpu-spike.sh XaJktdYiva2QsDL2pyJxmzQCeNdUgELdNvfDjy6JNiCCTZHH4KjMnh 900
4. pull prover.log gpu-util.csv rate-summary.txt -> bench-results/gpu/; TERMINATE.
Burner address above is throwaway (this-session scratchpad seed, discarded).
