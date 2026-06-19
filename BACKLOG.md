# BACKLOG

## Bug: Slow response on Status and Activity
Both the Status and Activity readouts in herdr seem to lag behidn what is actually happening with the agent. Often times
staying on processing while the task has compelted. Activity often lags behind or does not repreesnt the token
consumotion or what is going on. 

## Bug: The $/hr is too small
I often see values of 6000/h or 4000/h. It seems to me that those should be the red values

## Bug: Context should reset on /clear
using /clear on an agent window should reflect in herdr that the context has been cleared

## Feature: Claude window border
Can tmux add a border to the claude window just as herdr has it's own window title and border ie 
-herdr-----

## Feature: Persistent cost
A value that reflects the cost over multiple (all) sessions. For the repo collection as a whole and individual projects. 

## Feature: Quickstart
A script to quickly launch tmux and start herdr
tmux attach -t work
cargo run --release -- "/mnt/c/Users/Ben Bracamonte/Work"