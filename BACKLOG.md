# BACKLOG

## Bug: Auto Height - DONE
IF the parent project has a lot of repos, then the herder bar can extend too far when trying to start a chat window.
The auto heigh feature should not count unactive repos, or repos without any active agents in them. The auto hieght features
should only make room for showing repos with active agents, leeaving the rest of the screen space to the change window

## Feature: Tabbed Agents
When an agent is active in a project, I want it indnetd so I can clearly see that the agent is under that project.
If an agent opens sub agents under it's perview, if possible I want those indended in that agent which is indented 
under the project

## Bug: Status and Context is sometimes incorrect
I select a row in herdr, I press n to start an agent, it sarts and in herdr is report 15% context used up and processing already?
I haven't even asked a question yet

## Feature: Better colors
The hightlight row color is too bright and hard to read. The colors are overall could use some work. 
Build a theme system were we can slot in different colors for the UI. The default color should resemble that
of Dracula Dark

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

## Feature: Mouse Focus
Is this possible without betraying our design doc? I want to be able to click between the herdr window and the chat window

## Features: Charts
