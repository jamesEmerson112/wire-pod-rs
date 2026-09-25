//! The receive loop of the connect-time `robot_state` stream, which stores the
//! robot's latest state in the session's
//! [`StateSlot`](crate::robot::observe::StateSlot) and logs only the changes
//! that answer a question.
