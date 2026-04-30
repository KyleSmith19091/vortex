pub struct RunningInvocation {
    /// execution_id is a UUID representing the unique invocation of a service
    pub invocation_id: u128,

    /// name of the service 
    pub name: String,

    /// PID of the process handling the invocation (not unique as multiple invocations can be scheduled on a single process)
    pub pid: i32, 
}