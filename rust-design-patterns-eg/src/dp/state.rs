// This trait defines what a traffic light state *can* do
use std::mem;

trait TrafficLightState {
    fn next_state(self: Box<Self>) -> Box<dyn TrafficLightState>;
    fn status(&self);
}
// Our first Concrete State: Red
struct RedLight;
impl TrafficLightState for RedLight {
    fn next_state(self: Box<Self>) -> Box<dyn TrafficLightState> {
        println!("Changing from Red to Green... 🚦");
        Box::new(GreenLight)
    }
    fn status(&self) {
        println!("Light is RED. STOP! 🛑");
    }
}
// Our next Concrete State: Green
struct GreenLight;
impl TrafficLightState for GreenLight {
    fn next_state(self: Box<Self>) -> Box<dyn TrafficLightState> {
        println!("Changing from Green to Yellow... 🚦");
        Box::new(YellowLight)
    }
    fn status(&self) {
        println!("Light is GREEN. GO! 🟢");
    }
}
// And finally, the Yellow state
struct YellowLight;
impl TrafficLightState for YellowLight {
    fn next_state(self: Box<Self>) -> Box<dyn TrafficLightState> {
        println!("Changing from Yellow to Red... 🚦");
        Box::new(RedLight)
    }
    fn status(&self) {
        println!("Light is YELLOW. PREPARE TO STOP! 🟠");
    }
}
// This is our main TrafficLight struct, it holds the current state
struct TrafficLight {
    state: Box<dyn TrafficLightState>,
}
impl TrafficLight {
    fn new() -> Self {
        TrafficLight {
            state: Box::new(RedLight),
        } // Starts red, of course!
    }
    fn change_state(&mut self) {
        // next_state() consumes the old state (self: Box<Self>), so it can't
        // be called through &mut self directly. Swap the box out with a
        // placeholder first, then transition from the value we now own.
        let old_state = mem::replace(&mut self.state, Box::new(RedLight));
        self.state = old_state.next_state();
    }
    fn report_status(&self) {
        self.state.status();
    }
}
fn main() {
    let mut light = TrafficLight::new();
    light.report_status(); // Light is RED. STOP!
    light.change_state(); // Changing from Red to Green...
    light.report_status(); // Light is GREEN. GO!
    light.change_state(); // Changing from Green to Yellow...
    light.report_status(); // Light is YELLOW. PREPARE TO STOP!
    light.change_state(); // Changing from Yellow to Red...
    light.report_status(); // Light is RED. STOP!
}
