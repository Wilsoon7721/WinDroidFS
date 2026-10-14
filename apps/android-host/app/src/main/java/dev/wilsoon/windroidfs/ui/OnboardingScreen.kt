package dev.wilsoon.windroidfs.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

@Composable
fun OnboardingScreen(onOpenAccessSettings: () -> Unit, onCheckAgain: () -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(WinDroidTheme.colors.background)
            .systemBarsPadding()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally
    ) {
        Spacer(modifier = Modifier.height(32.dp))
        OnboardingHero()
        Spacer(modifier = Modifier.height(32.dp))
        OnboardingSteps()
        Spacer(modifier = Modifier.height(32.dp))
        OnboardingActions(onOpenAccessSettings, onCheckAgain)
    }
}

// Lock icon with headline and explanation
@Composable
private fun OnboardingHero() {
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .size(88.dp)
            .clip(CircleShape)
            .background(WinDroidTheme.colors.surface)
    ) {
        Icon(
            imageVector = Icons.Filled.Lock,
            contentDescription = null,
            tint = WinDroidTheme.colors.brand,
            modifier = Modifier.size(40.dp)
        )
    }
    Spacer(modifier = Modifier.height(24.dp))
    Text(
        text = "All Files Access Required",
        color = WinDroidTheme.colors.textPrimary,
        fontSize = 26.sp,
        fontWeight = FontWeight.Bold,
        textAlign = TextAlign.Center
    )
    Spacer(modifier = Modifier.height(12.dp))
    Text(
        text = "WinDroidFS turns your phone into a drive in Windows File Explorer. To do that, it requires access to all files on your device, not just photos and media.",
        color = WinDroidTheme.colors.textSecondary,
        fontSize = 15.sp,
        textAlign = TextAlign.Center,
        lineHeight = 22.sp
    )
}

// Step-by-step instructions
@Composable
private fun OnboardingSteps() {
    WinDroidCard {
        Text(
            text = "How to enable it",
            color = WinDroidTheme.colors.textPrimary,
            fontSize = 16.sp,
            fontWeight = FontWeight.SemiBold
        )
        Spacer(modifier = Modifier.height(16.dp))
        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            SetupStep(1, "Tap \"Open Settings\" below.")
            SetupStep(2, "Find WinDroidFS and turn on \"Allow access to manage all files\".")
            SetupStep(3, "Come back here. The app continues automatically.")
        }
    }
    Spacer(modifier = Modifier.height(16.dp))
    Box(
        modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp))
            .background(WinDroidTheme.colors.surfaceVariant).padding(16.dp)
    ) {
        Text(
            text = "Files are only shared with a PC you connect yourself. You can limit which folders are visible later in Settings.",
            color = WinDroidTheme.colors.textSecondary,
            fontSize = 13.sp,
            lineHeight = 19.sp
        )
    }
}

@Composable
private fun OnboardingActions(onOpenAccessSettings: () -> Unit, onCheckAgain: () -> Unit) {
    Button(
        onClick = onOpenAccessSettings,
        shape = RoundedCornerShape(14.dp),
        contentPadding = PaddingValues(vertical = 16.dp),
        colors = ButtonDefaults.buttonColors(
            containerColor = WinDroidTheme.colors.brand,
            contentColor = WinDroidTheme.colors.onAccent
        ),
        modifier = Modifier.fillMaxWidth()
    ) {
        Text(text = "Open Settings", fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
    }
    Spacer(modifier = Modifier.height(8.dp))
    TextButton(onClick = onCheckAgain, modifier = Modifier.fillMaxWidth()) {
        Text(text = "I've already allowed it", color = WinDroidTheme.colors.textSecondary)
    }
}

@Preview(name = "Onboarding - Light", showSystemUi = true)
@Composable
private fun OnboardingLightPreview() {
    WinDroidTheme(darkTheme = false) {
        OnboardingScreen(onOpenAccessSettings = {}, onCheckAgain = {})
    }
}

@Preview(name = "Onboarding - Dark", showSystemUi = true)
@Composable
private fun OnboardingDarkPreview() {
    WinDroidTheme(darkTheme = true) {
        OnboardingScreen(onOpenAccessSettings = {}, onCheckAgain = {})
    }
}
