package dev.wilsoon.windroidfs.ui

import android.content.Intent
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.wilsoon.windroidfs.ui.theme.WinDroidTheme

object ProjectLinks {
    const val REPO = "https://github.com/Wilsoon7721/WinDroidFS"
    const val WINDOWS_DOWNLOAD = "$REPO/releases/latest"
    val WEBSITE: String? = null
}

// Shown on the home screen until opened or dismissed
@Composable
fun WelcomeBanner(onOpen: () -> Unit, onDismiss: () -> Unit, modifier: Modifier = Modifier) {
    val colors = WinDroidTheme.colors
    val shape = RoundedCornerShape(16.dp)
    Row(
        verticalAlignment = Alignment.Top,
        modifier = modifier
            .fillMaxWidth()
            .clip(shape)
            .background(colors.surface)
            .background(colors.brand.copy(alpha = if (colors.isDark) 0.14f else 0.08f))
            .border(1.dp, colors.brand.copy(alpha = 0.45f), shape)
            .clickable(onClick = onOpen)
            .padding(start = 16.dp, top = 16.dp, bottom = 16.dp, end = 4.dp)
    ) {
        Icon(
            imageVector = Icons.Outlined.Info,
            contentDescription = null,
            tint = colors.brand,
            modifier = Modifier.padding(top = 2.dp).size(22.dp)
        )
        Spacer(modifier = Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f).padding(top = 1.dp)) {
            Text(
                text = "Hey!",
                color = colors.brand,
                fontSize = 17.sp,
                fontWeight = FontWeight.Bold
            )
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                text = "This seems like your first install. Click here to learn more about how to use this app.",
                color = colors.textPrimary,
                fontSize = 14.sp,
                lineHeight = 20.sp
            )
        }
        IconButton(onClick = onDismiss, modifier = Modifier.offset(y = (-8).dp)) {
            Icon(Icons.Default.Close, contentDescription = "Dismiss", tint = colors.textSecondary)
        }
    }
}

@Composable
fun WelcomeScreen(onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val uriHandler = LocalUriHandler.current
    val context = LocalContext.current
    val shareDownload = {
        val send = Intent(Intent.ACTION_SEND)
            .setType("text/plain")
            .putExtra(Intent.EXTRA_TEXT, "WinDroidFS for Windows: ${ProjectLinks.WINDOWS_DOWNLOAD}")
        context.startActivity(Intent.createChooser(send, "Send the download link to your PC"))
    }

    Column(modifier = Modifier.fillMaxSize().background(WinDroidTheme.colors.background).systemBarsPadding()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().padding(start = 8.dp, end = 24.dp, top = 16.dp, bottom = 8.dp)
        ) {
            IconButton(onClick = onBack) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back",
                    tint = WinDroidTheme.colors.textPrimary
                )
            }
            Spacer(modifier = Modifier.width(4.dp))
            Text(
                text = "Getting started",
                color = WinDroidTheme.colors.textPrimary,
                fontSize = 26.sp,
                fontWeight = FontWeight.Bold
            )
        }

        Column(
            modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())
                .padding(start = 24.dp, end = 24.dp, top = 8.dp, bottom = 32.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)
        ) {
            BodyText(
                "WinDroidFS puts your phone's storage on your Windows PC as a drive, so you can open, copy and move files in File Explorer like on any other disk."
            )

            SectionLabel("You'll need a PC", modifier = Modifier.padding(top = 12.dp))
            WinDroidCard {
                BodyText(
                    "This app is only half of WinDroidFS. The other half runs on a Windows 10 or 11 PC, and that's where your files show up. Install it there first."
                )
                Spacer(modifier = Modifier.height(16.dp))
                Button(
                    onClick = { uriHandler.openUri(ProjectLinks.WINDOWS_DOWNLOAD) },
                    shape = RoundedCornerShape(12.dp),
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text("Download for Windows", fontWeight = FontWeight.SemiBold)
                }
                LinkButton("Send the link to your PC", onClick = shareDownload)
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    text = "Or type this into the PC's browser:",
                    color = WinDroidTheme.colors.textSecondary,
                    fontSize = 13.sp
                )
                SelectionContainer {
                    Text(
                        text = ProjectLinks.WINDOWS_DOWNLOAD.removePrefix("https://"),
                        color = WinDroidTheme.colors.textPrimary,
                        fontSize = 13.sp,
                        fontFamily = FontFamily.Monospace
                    )
                }
            }

            SectionLabel("Then connect", modifier = Modifier.padding(top = 12.dp))
            WinDroidCard {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    SetupStep(1, "Open WinDroidFS on the PC. It sits in the tray, by the clock.")
                    SetupStep(2, "Plug your phone into the PC with a USB cable. Your storage appears in File Explorer as a new drive.")
                    SetupStep(3, "After that first time, you can also connect over Wi-Fi. See Local Wi-Fi on the home screen.")
                }
            }

            SectionLabel("Learn more", modifier = Modifier.padding(top = 12.dp))
            WinDroidCard {
                BodyText("Setup help, connection methods, and how your files are kept private.")
                Spacer(modifier = Modifier.height(8.dp))
                ProjectLinks.WEBSITE?.let { LinkButton("Website", onClick = { uriHandler.openUri(it) }) }
                LinkButton("GitHub", onClick = { uriHandler.openUri(ProjectLinks.REPO) })
            }
        }
    }
}

@Composable
private fun BodyText(text: String) {
    Text(text = text, color = WinDroidTheme.colors.textPrimary, fontSize = 15.sp, lineHeight = 22.sp)
}

@Composable
private fun LinkButton(text: String, onClick: () -> Unit) {
    OutlinedButton(
        onClick = onClick,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, WinDroidTheme.colors.brand),
        modifier = Modifier.fillMaxWidth().padding(top = 8.dp)
    ) {
        Text(text = text, color = WinDroidTheme.colors.brand, fontWeight = FontWeight.SemiBold)
    }
}

@Preview(name = "Welcome banner", widthDp = 380, backgroundColor = 0xFF0B1120, showBackground = true)
@Composable
private fun WelcomeBannerPreview() {
    WinDroidTheme(darkTheme = true) {
        WelcomeBanner(onOpen = {}, onDismiss = {}, modifier = Modifier.padding(16.dp))
    }
}

@Preview(name = "Getting started - Dark", showSystemUi = true)
@Composable
private fun WelcomeScreenPreview() {
    WinDroidTheme(darkTheme = true) { WelcomeScreen(onBack = {}) }
}
